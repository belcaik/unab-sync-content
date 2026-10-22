//! The one resumable-transfer primitive: GET with a `Range` resume off a
//! `.part` sidecar, streamed to disk, published by atomic rename.
//!
//! Both downloaders (Canvas files, Zoom recordings) go through
//! [`fetch_resumable`] so the resume policy lives in exactly one place: a
//! non-empty part file triggers a `Range` request, the body is appended only
//! when the server answers `206`, and a `200` — the server ignored the range —
//! truncates the part and starts over.

use crate::core::fsutil::atomic_rename;
use crate::core::http::HttpCtx;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, RANGE};
use std::path::Path;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("GET failed with status {0}")]
    Status(u16),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Downloads `url` to `dest`, resuming from the `.part` sidecar when one sits
/// next to it.
///
/// `part` is taken explicitly so each caller keeps its own on-disk naming
/// (Canvas uses `dest.with_extension("part")`, Zoom
/// `dest.with_extension("mp4.part")`). A non-empty part file triggers a
/// `Range: bytes=<len>-` request; the body is appended only on a `206`, while
/// a `200` means the server ignored the range and the part is truncated and
/// written from scratch — never appended to, which would leave a corrupt gap.
/// On success the part is published to `dest` with
/// [`crate::core::fsutil::atomic_rename`].
pub async fn fetch_resumable(
    http: &HttpCtx,
    url: &str,
    headers: HeaderMap,
    part: &Path,
    dest: &Path,
) -> Result<(), TransferError> {
    let resume_from = tokio::fs::metadata(part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut req = http.client.get(url).headers(headers);
    if resume_from > 0 {
        req = req.header(RANGE, format!("bytes={resume_from}-"));
    }
    let resp = http.send(req).await?;
    let status = resp.status();
    if !(status.is_success() || status.as_u16() == 206) {
        return Err(TransferError::Status(status.as_u16()));
    }

    // A 200 in response to a Range request means the server ignored it and is
    // sending the whole body, so the partial file must not be appended to.
    let appending = resume_from > 0 && status.as_u16() == 206;
    if let Some(parent) = part.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(appending)
        .truncate(!appending)
        .write(true)
        .open(part)
        .await?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    // Close the handle before the rename so this also works on Windows.
    drop(file);

    atomic_rename(part, dest).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinHandle;

    const BODY: &[u8] = b"hello zoom world";

    /// How the mock answers a GET.
    enum Mode {
        /// Answer 206 with the requested suffix when a `Range` header arrives.
        HonorRange,
        /// Answer 200 with the full body even when a `Range` header arrives.
        IgnoreRange,
        /// Answer this status with an empty body.
        Reject(u16),
    }

    struct MockServer {
        url: String,
        /// The `Range` header of each request received, in order.
        ranges: Arc<Mutex<Vec<Option<String>>>>,
        handle: JoinHandle<()>,
    }

    async fn start_mock(mode: Mode) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let recorded = ranges.clone();
        let handle = tokio::spawn(async move {
            // Each test makes exactly one request.
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let request = read_request(&mut stream).await;
            let range = range_header(&request);
            recorded.lock().unwrap().push(range.clone());
            match mode {
                Mode::HonorRange => {
                    if let Some(start) = range.as_deref().and_then(range_start) {
                        let body = &BODY[start..];
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\n\
                             Content-Length: {}\r\n\
                             Content-Range: bytes {}-{}/{}\r\n\
                             Connection: close\r\n\r\n",
                            body.len(),
                            start,
                            BODY.len() - 1,
                            BODY.len()
                        );
                        stream.write_all(head.as_bytes()).await.unwrap();
                        stream.write_all(body).await.unwrap();
                    } else {
                        write_full_body(&mut stream).await;
                    }
                }
                Mode::IgnoreRange => write_full_body(&mut stream).await,
                Mode::Reject(code) => {
                    let head = format!(
                        "HTTP/1.1 {code} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    stream.write_all(head.as_bytes()).await.unwrap();
                }
            }
        });
        MockServer {
            url: format!("http://{addr}/recording.mp4"),
            ranges,
            handle,
        }
    }

    async fn write_full_body(stream: &mut TcpStream) {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            BODY.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(BODY).await.unwrap();
    }

    /// Reads one request's head (request line + headers) off the wire.
    async fn read_request(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn range_header(request: &str) -> Option<String> {
        request.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("range")
                .then(|| value.trim().to_string())
        })
    }

    /// Parses the `N` out of a `bytes=N-...` range header.
    fn range_start(range: &str) -> Option<usize> {
        let (start, _) = range.strip_prefix("bytes=")?.split_once('-')?;
        start.parse().ok()
    }

    fn test_ctx() -> HttpCtx {
        // max_rps 0 disables pacing, so tests are not slowed by the limiter.
        let cfg = Config {
            max_rps: 0,
            ..Config::default()
        };
        HttpCtx::new(&cfg, reqwest::Client::new())
    }

    async fn run_fetch(url: &str, part: &Path, dest: &Path) -> Result<(), TransferError> {
        let ctx = test_ctx();
        tokio::time::timeout(
            Duration::from_secs(10),
            fetch_resumable(&ctx, url, HeaderMap::new(), part, dest),
        )
        .await
        .expect("fetch timed out")
    }

    async fn join(server: MockServer) {
        tokio::time::timeout(Duration::from_secs(5), server.handle)
            .await
            .expect("server task hung")
            .unwrap();
    }

    #[tokio::test]
    async fn fresh_download_writes_the_full_body() {
        let server = start_mock(Mode::HonorRange).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let part = dir.path().join("rec.mp4.part");

        run_fetch(&server.url, &part, &dest).await.unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), BODY);
        assert!(!part.exists(), "part file must be renamed away");
        assert_eq!(*server.ranges.lock().unwrap(), vec![None]);
        join(server).await;
    }

    #[tokio::test]
    async fn resume_appends_when_the_server_honours_the_range() {
        let server = start_mock(Mode::HonorRange).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let part = dir.path().join("rec.mp4.part");
        tokio::fs::write(&part, &BODY[..6]).await.unwrap();

        run_fetch(&server.url, &part, &dest).await.unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), BODY);
        assert!(!part.exists());
        assert_eq!(
            *server.ranges.lock().unwrap(),
            vec![Some("bytes=6-".to_string())]
        );
        join(server).await;
    }

    #[tokio::test]
    async fn resume_truncates_when_the_server_ignores_the_range() {
        let server = start_mock(Mode::IgnoreRange).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let part = dir.path().join("rec.mp4.part");
        // Stale garbage the same length as a real prefix: a zero-filled gap or
        // an append would both corrupt the final bytes.
        tokio::fs::write(&part, b"xxxxxx").await.unwrap();

        run_fetch(&server.url, &part, &dest).await.unwrap();

        assert_eq!(tokio::fs::read(&dest).await.unwrap(), BODY);
        join(server).await;
    }

    #[tokio::test]
    async fn non_success_status_leaves_no_final_file() {
        let server = start_mock(Mode::Reject(403)).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("rec.mp4");
        let part = dir.path().join("rec.mp4.part");

        let err = run_fetch(&server.url, &part, &dest).await.unwrap_err();

        assert!(matches!(err, TransferError::Status(403)), "got {err:?}");
        assert!(!dest.exists());
        assert!(!part.exists());
        join(server).await;
    }
}
