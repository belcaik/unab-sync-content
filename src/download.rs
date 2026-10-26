//! The single downloader for Canvas-hosted files.
//!
//! Owns one policy for conditional fetching (ETag), resumable transfers (`.part`
//! plus `Range`), atomic publication, and [`State`] bookkeeping. Callers choose
//! only where the file lands.

use crate::canvas::FileObj;
use crate::fsutil::atomic_rename;
use crate::http::HttpCtx;
use crate::layout;
use crate::state::{ItemState, State};
use futures_util::StreamExt;
use reqwest::header;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
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

/// Where a download should land.
pub enum Dest<'a> {
    /// The caller has already chosen the full path, extension included.
    Exact(&'a Path),
    /// Place the file in `dir` under `name` (named by
    /// [`crate::layout::attachment_file`]), deriving a missing extension from
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

/// The paths one run has handed out, so two Canvas files never share one.
///
/// Layout v2 names collapse case and punctuation (`Guía 1.pdf` and
/// `guia_1.PDF` are both `guia-1.pdf`), so two files of one directory can
/// ask for the same path. [`Claims::claim`] settles it without ever moving a
/// file that is already named: see there. Keep one `Claims` per run over the
/// directories it writes.
#[derive(Debug, Default)]
pub struct Claims {
    by_path: HashMap<PathBuf, u64>,
}

impl Claims {
    /// Where Canvas file `id` lands, given the path its name asks for.
    ///
    /// The first free one, this run, of: the name `state` recorded for the
    /// file ([`ItemState::file_name`]) — or, for an entry from before names
    /// were recorded that did download, `wanted` itself; for a file never
    /// downloaded, `wanted` unless something is already on disk there; then
    /// `wanted` with `-<id>` ([`layout::with_suffix`]), `-<id>-2`… Only file
    /// `id` is ever written to a `-<id>` name, so a file that already has its
    /// name keeps it while others with the same slug come and go, and nothing
    /// already on disk is overwritten by a different file.
    pub fn claim(&mut self, state: &State, id: u64, wanted: PathBuf) -> PathBuf {
        let prev = state.get(&state_key(id));
        let mut first = None;
        match prev.and_then(|p| p.file_name.as_deref()) {
            Some(name) => first = Some(wanted.with_file_name(name)),
            None if prev.is_some_and(|p| p.size.is_some()) || !wanted.exists() => {
                first = Some(wanted.clone());
            }
            None => {}
        }
        let name = wanted
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let suffixed = (1u32..).map(|n| {
            let suffix = if n == 1 {
                id.to_string()
            } else {
                format!("{id}-{n}")
            };
            wanted.with_file_name(layout::with_suffix(&name, &suffix))
        });
        let chosen = first
            .into_iter()
            .chain(suffixed)
            .find(|p| self.by_path.get(p).is_none_or(|&owner| owner == id))
            .unwrap_or(wanted);
        self.by_path.insert(chosen.clone(), id);
        chosen
    }
}

/// Downloads `f` unless the local copy is already current, returning its path.
///
/// The path is the one `claims` gives it ([`Claims::claim`]), so it can
/// differ from what `dest` asked for; its file name is recorded in `state`.
/// Skips the transfer when the stored ETag still matches and the file is on
/// disk. Partial transfers resume from the `.part` sidecar.
pub async fn download_if_needed(
    httpctx: &HttpCtx,
    f: &FileObj,
    dest: Dest<'_>,
    state: &mut State,
    claims: &mut Claims,
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
            dir.join(layout::attachment_file(&with_extension(name, ext)))
        }
    };
    let dest = claims.claim(state, f.id, dest);
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string);

    if let (Some(prev), Some(et)) = (state.items.get_mut(&key), etag.as_deref()) {
        if prev.etag.as_deref() == Some(et) && dest.exists() {
            info!(file_id = f.id, path = %dest.display(), "unchanged (etag)");
            if prev.file_name.is_none() {
                prev.file_name = file_name;
            }
            return Ok(dest);
        }
    }

    let part = dest.with_extension("part");
    let resume_from = tokio::fs::metadata(&part)
        .await
        .map(|m| m.len())
        .unwrap_or(0);

    let mut req = httpctx.client.get(url);
    if resume_from > 0 {
        req = req.header(header::RANGE, format!("bytes={resume_from}-"));
    }
    let resp = httpctx.send(req).await?;
    let status = resp.status();
    if !(status.is_success() || status.as_u16() == 206) {
        return Err(DownloadError::Status(status.as_u16()));
    }

    // A 200 in response to a Range request means the server ignored it and is
    // sending the whole body, so the partial file must not be appended to.
    let appending = resume_from > 0 && status.as_u16() == 206;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(appending)
        .truncate(!appending)
        .write(true)
        .open(&part)
        .await?;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        file.write_all(&chunk?).await?;
    }
    file.flush().await?;
    atomic_rename(&part, &dest).await?;
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
            file_name,
            path: None,
        },
    );
    Ok(dest)
}

fn header_str(resp: &reqwest::Response, name: header::HeaderName) -> Option<&str> {
    resp.headers().get(name).and_then(|h| h.to_str().ok())
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

    fn downloaded_as(name: Option<&str>) -> ItemState {
        ItemState {
            etag: Some("e".into()),
            updated_at: None,
            size: Some(1),
            content_hash: None,
            last_error: None,
            error_count: None,
            file_name: name.map(str::to_string),
            path: None,
        }
    }

    #[test]
    fn two_files_asking_for_one_path_get_two_paths() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("guia-1.pdf");
        let mut claims = Claims::default();
        let state = State::default();
        assert_eq!(claims.claim(&state, 22, wanted.clone()), wanted);
        assert_eq!(
            claims.claim(&state, 23, wanted.clone()),
            dir.path().join("guia-1-23.pdf")
        );
        // The same file asked again keeps its path.
        assert_eq!(claims.claim(&state, 22, wanted.clone()), wanted);
    }

    #[test]
    fn a_file_keeps_its_recorded_name_whoever_comes_first() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("guia-1.pdf");
        std::fs::write(&wanted, "22").unwrap();
        std::fs::write(dir.path().join("guia-1-23.pdf"), "23").unwrap();
        let mut state = State::default();
        state.set(state_key(22), downloaded_as(Some("guia-1.pdf")));
        state.set(state_key(23), downloaded_as(Some("guia-1-23.pdf")));
        let mut claims = Claims::default();
        assert_eq!(
            claims.claim(&state, 23, wanted.clone()),
            dir.path().join("guia-1-23.pdf")
        );
        assert_eq!(claims.claim(&state, 22, wanted.clone()), wanted);
    }

    #[test]
    fn a_new_file_never_takes_a_name_already_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("guia-1.pdf");
        std::fs::write(&wanted, "someone else").unwrap();
        let mut claims = Claims::default();
        assert_eq!(
            claims.claim(&State::default(), 23, wanted),
            dir.path().join("guia-1-23.pdf")
        );
    }

    #[test]
    fn an_entry_from_before_names_were_recorded_owns_the_plain_name() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("guia-1.pdf");
        std::fs::write(&wanted, "22").unwrap();
        let mut state = State::default();
        state.set(state_key(22), downloaded_as(None));
        let mut claims = Claims::default();
        assert_eq!(claims.claim(&state, 22, wanted.clone()), wanted);
    }

    #[test]
    fn a_failed_first_attempt_does_not_own_a_name_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("guia-1.pdf");
        std::fs::write(&wanted, "someone else").unwrap();
        let mut state = State::default();
        state.record_error(state_key(23), &"boom");
        let mut claims = Claims::default();
        assert_eq!(
            claims.claim(&state, 23, wanted),
            dir.path().join("guia-1-23.pdf")
        );
    }

    #[test]
    fn state_key_is_independent_of_the_discovering_feature() {
        assert_eq!(state_key(7), "file:7");
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
}
