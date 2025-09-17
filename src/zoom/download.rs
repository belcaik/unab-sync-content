use crate::config::Config;
use crate::ffmpeg::{download_via_ffmpeg, ensure_ffmpeg_available};
use crate::fsutil::sanitize_filename_preserve_ext;
use crate::zoom::api::effective_user_agent;
use crate::zoom::models::ZoomRecordingFile;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

pub async fn download_files(
    cfg: &Config,
    course_id: u64,
    files: Vec<ZoomRecordingFile>,
    concurrency: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    if files.is_empty() {
        println!("No hay archivos para descargar.");
        return Ok(());
    }

    ensure_ffmpeg_available(&cfg.zoom.ffmpeg_path).await?;

    let base = PathBuf::from(&cfg.download_root)
        .join("Zoom")
        .join(course_id.to_string());

    let mut name_counts: HashMap<String, usize> = HashMap::new();
    let mut tasks = Vec::new();

    for file in files {
        let mut name = sanitize_filename_preserve_ext(file.filename_hint() + ".mp4");
        let count = name_counts.entry(name.clone()).or_insert(0);
        if *count > 0 {
            let stem = name.trim_end_matches(".mp4");
            name = format!("{}_{}.mp4", stem, count);
        }
        *count += 1;

        let dest = base.join(&name);
        let headers = vec![("User-Agent".to_string(), effective_user_agent(cfg))];
        tasks.push((file.play_url.clone(), headers, dest));
    }

    let ffmpeg_path = Arc::new(cfg.zoom.ffmpeg_path.clone());

    futures_util::stream::iter(tasks.into_iter().map(|(url, headers, dest)| {
        let ffmpeg = ffmpeg_path.clone();
        async move {
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            download_via_ffmpeg(&ffmpeg, &headers, &url, &dest).await
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
