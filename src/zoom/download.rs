use crate::config::Config;
use crate::ffmpeg::{download_via_ffmpeg, ensure_ffmpeg_available, FfmpegError};
use crate::fsutil::sanitize_filename_preserve_ext;
use crate::zoom::api::effective_user_agent;
use crate::zoom::db::ZoomDb;
use crate::zoom::models::{ReplayHeader, ZoomRecordingFile};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, RANGE};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

pub async fn download_files(
    cfg: &Config,
    db: &ZoomDb,
    course_id: u64,
    files: Vec<ZoomRecordingFile>,
    concurrency: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if files.is_empty() {
        println!("No hay archivos para descargar.");
        return Ok(());
    }

    ensure_ffmpeg_available(&cfg.zoom.ffmpeg_path).await?;

    let assets = db.load_replay_headers(course_id)?;

    let recordings = db.load_meeting_payloads(course_id)?;

    println!(
        "Iniciando descarga de {} archivos de grabación para el curso {} ({} con cabeceras capturadas, {} reuniones en total)",
        files.len(),
        course_id,
        assets.len(),
        recordings.len()
    );

    if assets.is_empty() {
        println!("No se capturaron cabeceras de descarga MP4. Ejecuta 'u_crawler zoom sniff-cdp' y presiona DESCARGAR en cada grabación antes de volver a intentar.");
        return Ok(());
    }

    let base = PathBuf::from(&cfg.download_root)
        .join("Zoom")
        .join(course_id.to_string());

    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let mut tasks = Vec::new();

    for file in files {
        let play_url = file.play_url.clone();
        let asset = match assets.get(&play_url) {
            Some(asset) => asset,
            None => {
                println!(
                    "No se encontró captura de headers para la grabación {}. Omite descarga.",
                    play_url
                );
                continue;
            }
        };

        let mut filename = sanitize_filename_preserve_ext(file.filename_hint() + ".mp4");
        let count = name_counts.entry(filename.clone()).or_insert(0);
        if *count > 0 {
            let stem = filename.trim_end_matches(".mp4");
            filename = format!("{}_{}.mp4", stem, count);
        }
        *count += 1;

        let dest = base.join(&filename);
        let headers = build_ffmpeg_headers(cfg, asset, &play_url);
        tasks.push((asset.download_url.clone(), headers, dest));
    }

    if tasks.is_empty() {
        println!("No hay descargas pendientes con headers capturados.");
        return Ok(());
    }

    let ffmpeg_path = Arc::new(cfg.zoom.ffmpeg_path.clone());

    futures_util::stream::iter(tasks.into_iter().map(|(url, headers, dest)| {
        let ffmpeg = ffmpeg_path.clone();
        async move {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            download_with_fallback(&ffmpeg, headers, url, dest).await
        }
    }))
    .buffer_unordered(concurrency.max(1))
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    println!("Descarga completa en {}", base.display());
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
            println!(
                "ffmpeg no pudo descargar {} ({}); intentando descarga HTTP directa...",
                url, err
            );
            http_download(&headers, &url, &dest).await
        }
        Err(other) => Err(Box::new(other)),
    }
}

async fn http_download(
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

    let mut request = client.get(url);
    request = request.headers(header_map);

    let response = request.send().await?;
    if !(response.status().is_success() || response.status().as_u16() == 206) {
        return Err(format!("HTTP {} al descargar {}", response.status(), url).into());
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

fn build_ffmpeg_headers(
    cfg: &Config,
    asset: &ReplayHeader,
    referer: &str,
) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    let lower_map: HashMap<String, String> = asset
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();

    // Preserve order roughly similar to browser critical headers
    let ordered_keys = [
        "host",
        "authority",
        "accept",
        "accept-language",
        "cookie",
        "cf-ipcountry",
        "sec-ch-ua",
        "sec-ch-ua-mobile",
        "sec-ch-ua-platform",
        "sec-fetch-dest",
        "sec-fetch-mode",
        "sec-fetch-site",
        "sec-fetch-storage-access",
        "range",
        "origin",
        "referer",
        "x-xsrf-token",
        "x-zm-aid",
        "x-zm-cluster-id",
        "x-zm-haid",
        "x-zm-region",
        "priority",
    ];

    for key in ordered_keys.iter() {
        if let Some(value) = lower_map.get(*key) {
            headers.push((canonical_header_name(key), value.clone()));
        }
    }

    for (key, value) in lower_map.iter() {
        if ordered_keys.contains(&key.as_str()) || should_skip_header(key) {
            continue;
        }
        headers.push((canonical_header_name(key), value.clone()));
    }

    apply_or_replace(&mut headers, "User-Agent", &effective_user_agent(cfg));
    apply_or_replace(&mut headers, "Referer", referer);
    ensure_header(&mut headers, "Accept", "*/*");
    ensure_header(&mut headers, "Range", "bytes=0-");

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
