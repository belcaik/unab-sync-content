use crate::config::Config;
use crate::ffmpeg::{download_via_ffmpeg, ensure_ffmpeg_available, FfmpegError};
use crate::fsutil::sanitize_filename_preserve_ext;
use crate::progress::progress_bar;
use crate::zoom::api::effective_user_agent;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ReplayHeader, ZoomRecordingFile};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tracing::{info, warn};

pub async fn download_files(
    cfg: &Config,
    db: &ZoomDb,
    course_id: u64,
    files: Vec<ZoomRecordingFile>,
    concurrency: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if files.is_empty() {
        println!("No recording files to download.");
        return Ok(());
    }

    ensure_ffmpeg_available(&cfg.zoom.ffmpeg_path).await?;

    let assets = db.load_replay_headers(course_id)?;

    let recordings = db.load_meeting_payloads(course_id)?;

    info!(
        course_id,
        total_files = files.len(),
        captured_headers = assets.len(),
        recordings = recordings.len(),
        "starting Zoom download batch"
    );

    if assets.is_empty() {
        println!(
            "No captured MP4 headers were found. Run `u_crawler zoom sniff-cdp` and replay each recording before retrying."
        );
        return Ok(());
    }

    let base = PathBuf::from(&cfg.download_root)
        .join("Zoom")
        .join(course_id.to_string());

    // Load cookies for downloads
    let cookies = db.load_cookies()?;
    
    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let mut tasks = Vec::new();

    // Filter `assets` to only include those that correspond to `files`
    for file in files {
        let asset = match assets.get(&file.play_url) {
            Some(asset) => asset,
            None => {
                warn!(
                    play_url = file.play_url,
                    "missing replay headers for recording; skipping download"
                );
                continue;
            }
        };

        let play_url = asset.download_url.clone();
        let mut filename = sanitize_filename_preserve_ext(file.filename_hint() + ".mp4");
        let count = name_counts.entry(filename.clone()).or_insert(0);
        if *count > 0 {
            let stem = filename.trim_end_matches(".mp4");
            filename = format!("{}_{}.mp4", stem, count);
        }
        *count += 1;

        let dest = base.join(&filename);
        let headers = build_ffmpeg_headers(cfg, asset, &file.play_url, &cookies, &asset.download_url);
        tasks.push((asset.download_url.clone(), headers, dest));
    }

    if tasks.is_empty() {
        println!("No downloads remain with captured headers.");
        return Ok(());
    }

    let ffmpeg_path = Arc::new(cfg.zoom.ffmpeg_path.clone());
    let progress = Arc::new(progress_bar(
        tasks.len() as u64,
        &format!("Downloading recordings for course {}", course_id),
    ));

    futures_util::stream::iter(tasks.into_iter().map(|(url, headers, dest)| {
        let ffmpeg = ffmpeg_path.clone();
        let pb = progress.clone();
        async move {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let result = download_with_fallback(&ffmpeg, headers, url, dest).await;
            pb.inc(1);
            result
        }
    }))
    .buffer_unordered(concurrency.max(1))
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    progress.finish_and_clear();

    println!("Download completed at {}", base.display());
    Ok(())
}

async fn download_with_fallback(
    ffmpeg_path: &str,
    headers: Vec<(String, String)>,
    url: String,
    dest: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    match download_via_ffmpeg(ffmpeg_path, &headers, &url, &dest).await {
        Ok(()) => Ok(()),
        Err(err @ FfmpegError::Process { .. }) => {
            warn!(url, error = %err, "ffmpeg download failed; falling back to HTTP");
            http_download(&headers, &url, &dest).await
        }
        Err(other) => Err(Box::new(other)),
    }
}

pub async fn http_download(
    headers: &[(String, String)],
    url: &str,
    dest: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?;

    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = temp_path(dest);
    let mut resume_from = 0u64;
    if let Ok(meta) = tokio::fs::metadata(&tmp).await {
        resume_from = meta.len();
    }

    let mut header_map = HeaderMap::new();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("range") {
            continue;
        }
        if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
            if let Ok(header_value) =
                HeaderValue::from_str(value).or_else(|_| HeaderValue::from_bytes(value.as_bytes()))
            {
                header_map.insert(header_name, header_value);
            }
        }
    }
    if resume_from > 0 {
        header_map.insert(
            RANGE,
            HeaderValue::from_str(&format!("bytes={}-", resume_from))?,
        );
    }

    // DEBUG: Log all headers being sent
    println!("HTTP download {} with {} headers:", url, header_map.len());
    for (k, v) in header_map.iter() {
        let val_str = v.to_str().unwrap_or("<binary>");
        let display_val = if val_str.len() > 100 {
            format!("{}...", &val_str[..100])
        } else {
            val_str.to_string()
        };
        println!("  {}: {}", k, display_val);
    }

    let mut request = client.get(url);
    request = request.headers(header_map);

    let response = request.send().await?;
    if !(response.status().is_success() || response.status().as_u16() == 206) {
        return Err(format!("HTTP {} while downloading {}", response.status(), url).into());
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&tmp)
        .await?;
    if resume_from > 0 {
        file.seek(std::io::SeekFrom::Start(resume_from)).await?;
    } else {
        file.set_len(0).await?;
    }

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let data = chunk?;
        file.write_all(&data).await?;
    }
    file.flush().await?;
    file.sync_data().await?;
    drop(file);

    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

fn temp_path(dest: &Path) -> PathBuf {
    dest.with_extension("mp4.part")
}

pub fn build_ffmpeg_headers(
    _cfg: &Config,
    asset: &ReplayHeader,
    _referer: &str,
    cookies: &[crate::zoom::models::ZoomCookie],
    download_url: &str,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = asset.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    
    // Extract domain from download URL
    let domain = if let Ok(url) = url::Url::parse(download_url) {
        url.host_str().unwrap_or("").to_string()
    } else {
        String::new()
    };
    
    // Build Cookie header from saved cookies matching the domain
    let mut cookie_values = Vec::new();
    for cookie in cookies {
        // Match cookies for this domain (ssrweb.zoom.us, zoom.us, etc.)
        if domain.ends_with(&cookie.domain) || cookie.domain.starts_with('.') && domain.ends_with(&cookie.domain[1..]) {
            cookie_values.push(format!("{}={}", cookie.name, cookie.value));
        }
    }
    
    if !cookie_values.is_empty() {
        let cookie_header = cookie_values.join("; ");
        println!("Building Cookie header with {} cookies for domain {}", cookie_values.len(), domain);
        headers.push(("Cookie".to_string(), cookie_header));
    } else {
        println!("⚠ Warning: No cookies found for domain {}", domain);
    }
    
    println!("Total headers for download (including Cookie): {}", headers.len());
    headers
}

fn ensure_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if !headers
        .iter()
        .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn apply_or_replace(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(entry) = headers
        .iter_mut()
        .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        entry.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

fn should_skip_header(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with(':')
        || name == "content-length"
        || name == "content-type"
        || name == "transfer-encoding"
        || name == "connection"
        || name == "keep-alive"
        || name == "upgrade"
}

fn canonical_header_name(name: &str) -> String {
    let mut result = String::new();
    for (idx, segment) in name.split('-').enumerate() {
        if idx > 0 {
            result.push('-');
        }
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            result.push(first.to_ascii_uppercase());
            for ch in chars {
                result.push(ch.to_ascii_lowercase());
            }
        }
    }
    result
}
