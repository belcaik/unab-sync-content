pub mod api;
pub mod cdp;
pub mod db;
pub mod download;
pub mod models;

use crate::config::{load_config_from_path, ConfigPaths};
use api::{ZoomApiError, ZoomClient};
use cdp::{sniff_cdp, SniffOptions};
use db::ZoomDb;
use models::{RecordingListResponse, RecordingSummary};
use std::error::Error;

pub async fn zoom_sniff_cdp(
    course_id: u64,
    debug_port: u16,
    keep_tab: bool,
) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let cfg = load_config_from_path(&paths.config_file).await?;
    let db = ZoomDb::new(&paths.config_dir)?;

    sniff_cdp(SniffOptions {
        course_id,
        debug_port,
        keep_tab,
        config: &cfg,
        db: &db,
    })
    .await?;

    match ZoomClient::new(&cfg, &db, course_id).await {
        Ok(client) => {
            match client.list_recordings(None).await {
                Ok(response) => {
                    if let Err(e) = db.save_meetings(course_id, &response) {
                        tracing::warn!(course_id, error = %e, "no se pudo persistir listado inicial tras sniff");
                    }
                    let count = response
                        .result
                        .as_ref()
                        .and_then(|r| r.list.as_ref())
                        .map(|l| l.len())
                        .unwrap_or(0);
                    println!(
                        "Capturadas {} reuniones inmediatamente después del sniff.",
                        count
                    );
                }
                Err(e) => {
                    tracing::warn!(course_id, error = %e, "falló fetch inicial de reuniones tras sniff");
                }
            }
        }
        Err(e) => {
            tracing::warn!(course_id, error = %e, "no se pudo crear cliente Zoom tras sniff");
        }
    }

    Ok(())
}

pub async fn zoom_list(
    course_id: u64,
    since: Option<String>,
    json: bool,
) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let cfg = load_config_from_path(&paths.config_file).await?;
    let db = ZoomDb::new(&paths.config_dir)?;

    let client = ZoomClient::new(&cfg, &db, course_id)
        .await
        .map_err(map_api_err)?;
    let response = client
        .list_recordings(since.as_deref())
        .await
        .map_err(map_api_err)?;
    db.save_meetings(course_id, &response)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        render_listing(&response);
    }
    Ok(())
}

pub async fn zoom_fetch_urls(course_id: u64) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let cfg = load_config_from_path(&paths.config_file).await?;
    let db = ZoomDb::new(&paths.config_dir)?;

    let client = ZoomClient::new(&cfg, &db, course_id)
        .await
        .map_err(map_api_err)?;

    let mut meetings = db.load_meeting_payloads(course_id)?;
    if meetings.is_empty() {
        let listing = client
            .list_recordings(None)
            .await
            .map_err(map_api_err)?;
        db.save_meetings(course_id, &listing)?;
        meetings = db.load_meeting_payloads(course_id)?;
    }

    if meetings.is_empty() {
        println!("No se encontraron reuniones para el curso {course_id}.");
        return Ok(());
    }

    let mut stored = 0usize;
    for payload in meetings {
        let summary: RecordingSummary = serde_json::from_value(payload)?;
        let files = client
            .fetch_recording_files(&summary)
            .await
            .map_err(map_api_err)?;
        if files.is_empty() {
            continue;
        }
        db.save_files(course_id, &summary.meeting_id, &files)?;
        println!(
            "Capturados {} playUrl(s) para meeting {}",
            files.len(),
            summary.meeting_id
        );
        stored += files.len();
    }

    if stored == 0 {
        println!("No se obtuvieron playUrl. ¿La herramienta permite descargas?");
    }
    Ok(())
}

pub async fn zoom_download(course_id: u64, concurrency: usize) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let cfg = load_config_from_path(&paths.config_file).await?;
    let db = ZoomDb::new(&paths.config_dir)?;
    let files = db.load_files(course_id)?;
    download::download_files(&cfg, course_id, files, concurrency).await
}

fn render_listing(response: &RecordingListResponse) {
    println!(
        "{:<20} | {:<20} | {:<40} | {:<15}",
        "Meeting ID", "Inicio", "Tema", "Zona"
    );
    println!("{}", "-".repeat(105));
    if let Some(result) = &response.result {
        if let Some(list) = &result.list {
            for item in list {
                println!(
                    "{:<20} | {:<20} | {:<40} | {:<15}",
                    item.meeting_id,
                    item.start_time.clone().unwrap_or_else(|| "?".into()),
                    item.topic.clone().unwrap_or_else(|| "(sin tema)".into()),
                    item.timezone.clone().unwrap_or_else(|| "".into())
                );
            }
        }
    }
}

fn map_api_err(err: ZoomApiError) -> Box<dyn Error> {
    match err {
        ZoomApiError::Db(e) => e,
        other => Box::new(other),
    }
}
