//! The single downloader for Canvas-hosted files.
//!
//! Owns one policy for conditional fetching (ETag), resumable transfers (`.part`
//! plus `Range`), atomic publication, and [`State`](crate::core::state::State) bookkeeping. Callers choose
//! only where the file lands.

use crate::canvas::{CanvasClient, FileObj};
use crate::core::fsutil::sanitize_filename_preserve_ext;
use crate::core::http::HttpCtx;
use crate::core::state::{ItemState, State};
use crate::core::transfer::{fetch_resumable, TransferError};
use reqwest::header;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{info, warn};

#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("file {0} has neither a download_url nor a url")]
    MissingUrl(u64),
    #[error("GET failed with status {0}")]
    Status(u16),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<TransferError> for DownloadError {
    fn from(e: TransferError) -> Self {
        match e {
            TransferError::Status(code) => Self::Status(code),
            TransferError::Http(e) => Self::Http(e),
            TransferError::Io(e) => Self::Io(e),
        }
    }
}

/// Where a download should land.
pub enum Dest<'a> {
    /// The caller has already chosen the full path, extension included.
    Exact(&'a Path),
    /// Place the file in `dir` under `name`, deriving a missing extension from
    /// the response's `Content-Type`.
    InDir { dir: &'a Path, name: &'a str },
}

/// State key for a Canvas file.
///
/// Deliberately independent of *which* feature discovered the file: the same
/// file reachable from both a module and an announcement is one item, fetched
/// once.
pub fn state_key(file_id: u64) -> String {
    format!("file:{file_id}")
}

/// Downloads `f` unless the local copy is already current, returning its path.
///
/// Skips the transfer when the stored ETag still matches and the file is on
/// disk. Partial transfers resume from the `.part` sidecar.
pub async fn download_if_needed(
    httpctx: &HttpCtx,
    f: &FileObj,
    dest: Dest<'_>,
    state: &mut State,
) -> Result<PathBuf, DownloadError> {
    let url = f
        .download_url
        .as_ref()
        .or(f.url.as_ref())
        .ok_or(DownloadError::MissingUrl(f.id))?;
    let key = state_key(f.id);

    let head = httpctx.send(httpctx.client.head(url)).await?;
    if !head.status().is_success() {
        warn!(file_id = f.id, status = %head.status().as_u16(), "HEAD non-success, continuing with GET");
    }
    let etag = header_str(&head, header::ETAG).map(|s| s.trim_matches('"').to_string());
    let advertised_size = header_str(&head, header::CONTENT_LENGTH)
        .and_then(|s| s.parse::<u64>().ok())
        .or(f.size);

    let dest = match dest {
        Dest::Exact(p) => p.to_path_buf(),
        Dest::InDir { dir, name } => {
            let ext = header_str(&head, header::CONTENT_TYPE).and_then(content_type_to_ext);
            dir.join(sanitize_filename_preserve_ext(with_extension(name, ext)))
        }
    };

    if let (Some(prev), Some(et)) = (state.get(&key), etag.as_deref()) {
        if prev.etag.as_deref() == Some(et) && dest.exists() {
            info!(file_id = f.id, path = %dest.display(), "unchanged (etag)");
            return Ok(dest);
        }
    }

    let part = dest.with_extension("part");
    fetch_resumable(httpctx, url, header::HeaderMap::new(), &part, &dest).await?;
    info!(file_id = f.id, path = %dest.display(), "downloaded");

    let size = tokio::fs::metadata(&dest)
        .await
        .map(|m| m.len())
        .ok()
        .or(advertised_size);
    state.set(
        key,
        ItemState {
            etag,
            updated_at: f.updated_at.clone(),
            size,
            content_hash: None,
            last_error: None,
            error_count: None,
        },
    );
    Ok(dest)
}

fn header_str(resp: &reqwest::Response, name: header::HeaderName) -> Option<&str> {
    resp.headers().get(name).and_then(|h| h.to_str().ok())
}

/// The preferred on-disk name for a Canvas file: its display name, else its
/// filename, else a synthesized `file_{id}`.
///
/// The extension may be absent here; [`Dest::InDir`] derives one from the
/// response `Content-Type` when it is.
pub fn preferred_name(f: &FileObj) -> String {
    f.display_name
        .clone()
        .or_else(|| f.filename.clone())
        .unwrap_or_else(|| format!("file_{}", f.id))
}

/// Fetches `file_id`'s metadata from Canvas, or records the failure against
/// the file's state key and returns `None`: one unavailable file must not
/// abandon its siblings.
pub async fn fetch_or_record(
    canvas: &CanvasClient,
    file_id: u64,
    state: &mut State,
) -> Option<FileObj> {
    match canvas.get_file(file_id).await {
        Ok(f) => Some(f),
        Err(e) => {
            warn!(file_id, error = %e, "file metadata fetch failed");
            state.record_error(state_key(file_id), &e);
            None
        }
    }
}

/// Downloads `f` to `dest` via [`download_if_needed`], or records the failure
/// against the file's state key and returns `None`.
pub async fn download_or_record(
    httpctx: &HttpCtx,
    f: &FileObj,
    dest: Dest<'_>,
    state: &mut State,
) -> Option<PathBuf> {
    match download_if_needed(httpctx, f, dest, state).await {
        Ok(path) => Some(path),
        Err(e) => {
            warn!(file_id = f.id, error = %e, "download failed");
            state.record_error(state_key(f.id), &e);
            None
        }
    }
}

/// Fetches `file_id`'s metadata and downloads the file into `dir` under its
/// [`preferred_name`]: the composition of [`fetch_or_record`] and
/// [`download_or_record`] for callers that hold only the file id.
pub async fn fetch_and_download(
    canvas: &CanvasClient,
    file_id: u64,
    dir: &Path,
    state: &mut State,
) -> Option<PathBuf> {
    let f = fetch_or_record(canvas, file_id, state).await?;
    download_or_record(
        &canvas.http,
        &f,
        Dest::InDir {
            dir,
            name: &preferred_name(&f),
        },
        state,
    )
    .await
}

/// Appends `ext` to `name` when `name` has no extension of its own.
fn with_extension(name: &str, ext: Option<&str>) -> String {
    match ext {
        Some(ext) if Path::new(name).extension().is_none() => format!("{name}.{ext}"),
        _ => name.to_string(),
    }
}

/// Maps a `Content-Type` to a file extension, for Canvas files served without one.
fn content_type_to_ext(ct: &str) -> Option<&'static str> {
    let mime = ct.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/x-ipynb+json" | "application/x-jupyter-notebook" => "ipynb",
        "application/json" => "json",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-tar" => "tar",
        "application/gzip" | "application/x-gzip" => "gz",
        "text/csv" => "csv",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/markdown" => "md",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[test]
    fn state_key_is_independent_of_the_discovering_feature() {
        assert_eq!(state_key(7), "file:7");
    }

    #[test]
    fn preferred_name_prefers_display_name_then_filename_then_id() {
        let mut f = FileObj {
            id: 7,
            display_name: Some("Shown.txt".into()),
            filename: Some("stored.txt".into()),
            size: None,
            updated_at: None,
            url: None,
            download_url: None,
        };
        assert_eq!(preferred_name(&f), "Shown.txt");
        f.display_name = None;
        assert_eq!(preferred_name(&f), "stored.txt");
        f.filename = None;
        assert_eq!(preferred_name(&f), "file_7");
    }

    #[test]
    fn extension_is_derived_only_when_missing() {
        assert_eq!(with_extension("taller", Some("ipynb")), "taller.ipynb");
        assert_eq!(with_extension("taller.ipynb", Some("json")), "taller.ipynb");
        assert_eq!(with_extension("taller", None), "taller");
    }

    #[test]
    fn content_type_ignores_charset_parameters() {
        assert_eq!(content_type_to_ext("text/csv; charset=utf-8"), Some("csv"));
    }

    #[test]
    fn unknown_content_type_yields_no_extension() {
        assert_eq!(content_type_to_ext("application/x-unknown"), None);
    }

    // --- fetch/download-or-record helpers, against a mock HTTP server in the
    // style of core::transfer's tests. ---

    const BODY: &[u8] = b"hello canvas!";

    /// Routes the mock's canned responses: file 7's metadata, its download
    /// URL's HEAD and GET, and a 404 for anything else.
    fn respond(path: &str, base: &str) -> (&'static str, Vec<u8>) {
        match path {
            "/api/v1/files/7" => (
                "200 OK",
                format!(r#"{{"id":7,"display_name":"notes.txt","url":"{base}/dl/notes"}}"#)
                    .into_bytes(),
            ),
            "/dl/notes" => ("200 OK", BODY.to_vec()),
            _ => ("404 Not Found", Vec::new()),
        }
    }

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

    /// Serves [`respond`] until the test runtime shuts the task down.
    async fn start_mock() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let served = base.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut stream).await;
                let mut parts = request.split_whitespace();
                let method = parts.next().unwrap_or("");
                let path = parts.next().unwrap_or("");
                let (status, body) = respond(path, &served);
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).await.unwrap();
                if method != "HEAD" {
                    stream.write_all(&body).await.unwrap();
                }
            }
        });
        base
    }

    async fn test_client(base: &str) -> CanvasClient {
        let mut cfg = Config {
            max_rps: 0,
            ..Config::default()
        };
        cfg.canvas.base_url = base.to_string();
        cfg.canvas.token = Some("test-token".to_string());
        CanvasClient::new(&cfg).await.unwrap()
    }

    #[tokio::test]
    async fn fetch_or_record_records_a_metadata_failure_instead_of_propagating() {
        let base = start_mock().await;
        let canvas = test_client(&base).await;
        let mut state = State::default();

        let got = fetch_or_record(&canvas, 9, &mut state).await;

        assert!(got.is_none());
        let item = state.get("file:9").expect("error recorded");
        assert_eq!(item.error_count, Some(1));
        assert!(item.last_error.is_some());
    }

    #[tokio::test]
    async fn download_or_record_records_a_failed_download_instead_of_propagating() {
        let base = start_mock().await;
        let canvas = test_client(&base).await;
        let mut state = State::default();
        let dir = tempfile::tempdir().unwrap();
        let missing = FileObj {
            id: 5,
            display_name: Some("gone.txt".into()),
            filename: None,
            size: None,
            updated_at: None,
            url: Some(format!("{base}/missing")),
            download_url: None,
        };

        let got = download_or_record(
            &canvas.http,
            &missing,
            Dest::InDir {
                dir: dir.path(),
                name: "gone.txt",
            },
            &mut state,
        )
        .await;

        assert!(got.is_none());
        let item = state.get("file:5").expect("error recorded");
        assert_eq!(item.error_count, Some(1));
        assert!(!dir.path().join("gone.txt").exists());
    }

    #[tokio::test]
    async fn fetch_and_download_lands_the_file_under_its_preferred_name() {
        let base = start_mock().await;
        let canvas = test_client(&base).await;
        let mut state = State::default();
        let dir = tempfile::tempdir().unwrap();

        let got = fetch_and_download(&canvas, 7, dir.path(), &mut state)
            .await
            .expect("download succeeded");

        assert_eq!(got, dir.path().join("notes.txt"));
        assert_eq!(tokio::fs::read(&got).await.unwrap(), BODY);
        assert_eq!(
            state.get("file:7").and_then(|i| i.size),
            Some(BODY.len() as u64)
        );
    }
}
