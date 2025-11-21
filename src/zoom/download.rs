use crate::config::Config;

use crate::zoom::models::ReplayHeader;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

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
    let mut headers: Vec<(String, String)> = asset
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

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
        if domain.ends_with(&cookie.domain)
            || cookie.domain.starts_with('.') && domain.ends_with(&cookie.domain[1..])
        {
            cookie_values.push(format!("{}={}", cookie.name, cookie.value));
        }
    }

    if !cookie_values.is_empty() {
        let cookie_header = cookie_values.join("; ");

        headers.push(("Cookie".to_string(), cookie_header));
    } else {
        println!("⚠ Warning: No cookies found for domain {}", domain);
    }

    headers
}
