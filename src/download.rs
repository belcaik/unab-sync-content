//! The single downloader for Canvas-hosted files.
//!
//! Owns one policy for conditional fetching (ETag), resumable transfers (`.part`
//! plus `Range`), atomic publication, and [`State`] bookkeeping. Callers choose
//! only where the file lands.

use crate::canvas::FileObj;
use crate::fsutil::{atomic_rename, sanitize_filename_preserve_ext};
use crate::http::HttpCtx;
use crate::state::{ItemState, State};
use futures_util::StreamExt;
use reqwest::header;
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
    /// Place the file in `dir` under `name`, deriving a missing extension from
    /// the response's `Content-Type`.
    InDir { dir: &'a Path, name: &'a str },
}

/// State key for a Canvas file.
///
/// Deliberately independent of *which* feature discovered the file: the same
/// file reachable from both a module and an announcement is one item, fetched
/// once.
fn state_key(file_id: u64) -> String {
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
