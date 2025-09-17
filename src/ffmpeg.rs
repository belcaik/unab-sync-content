use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum FfmpegError {
    #[error("ffmpeg binary not found at {0}")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("ffmpeg exited with status {code:?}: {message}")]
    Process { code: Option<i32>, message: String },
}

/// Run `ffmpeg -version` to ensure the binary is callable.
pub async fn ensure_ffmpeg_available(path: &str) -> Result<(), FfmpegError> {
    let mut cmd = Command::new(path);
    cmd.arg("-version");
    match cmd.output().await {
        Ok(output) => {
            if output.status.success() {
                Ok(())
            } else {
                Err(FfmpegError::Process {
                    code: output.status.code(),
                    message: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(FfmpegError::NotFound(path.to_string()))
        }
        Err(e) => Err(FfmpegError::Io(e)),
    }
}

/// Download the given media URL using ffmpeg with provided headers, writing to `dest` atomically.
pub async fn download_via_ffmpeg(
    path: &str,
    headers: &[(String, String)],
    input_url: &str,
    dest: &Path,
) -> Result<(), FfmpegError> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp = temp_path(dest);

    let mut header_blob = String::new();
    for (name, value) in headers {
        header_blob.push_str(name);
        header_blob.push_str(": ");
        header_blob.push_str(value);
        header_blob.push_str("\r\n");
    }

    let mut cmd = Command::new(path);
    cmd.arg("-y") // overwrite partial outputs
        .arg("-loglevel")
        .arg("error")
        .arg("-hide_banner")
        .arg("-headers")
        .arg(header_blob)
        .arg("-i")
        .arg(input_url)
        .arg("-c")
        .arg("copy")
        .arg("-map")
        .arg("0")
        .arg("-movflags")
        .arg("+faststart")
        .arg(tmp.as_os_str());

    match cmd.output().await {
        Ok(output) => {
            if output.status.success() {
                tokio::fs::rename(&tmp, dest).await?;
                Ok(())
            } else {
                let _ = tokio::fs::remove_file(&tmp).await;
                Err(FfmpegError::Process {
                    code: output.status.code(),
                    message: String::from_utf8_lossy(&output.stderr).into_owned(),
                })
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(FfmpegError::NotFound(path.to_string()))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(FfmpegError::Io(e))
        }
    }
}

fn temp_path(dest: &Path) -> PathBuf {
    dest.with_extension("mp4.part")
}
