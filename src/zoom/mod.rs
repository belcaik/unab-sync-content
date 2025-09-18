pub mod api;
pub mod cdp;
pub mod db;
pub mod download;
pub mod models;

use crate::config::{load_config_from_path, ConfigPaths};
use api::{ZoomApiError, ZoomClient};
use cdp::{capture_play_urls, sniff_cdp, CaptureOptions, SniffOptions};
use db::ZoomDb;
use models::{RecordingListResponse, RecordingSummary, RecordingsResult, ZoomRecordingFile};
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

    println!("Sniff completado.");
    // show db captured cookies
    let cookies = db.load_cookies()?;
    if cookies.is_empty() {
        println!("No se capturaron cookies Zoom.");
    } else {
        println!("Cookies Zoom capturadas:");
        for _cookie in &cookies {
            // println!("- {}: {}", cookie.name, cookie.value);
        }
    }
    // try to fetch initial listing

    match ZoomClient::new(&cfg, &db, course_id).await {
        Ok(client) => match client.list_recordings(None).await {
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
        },
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

    let (response, from_cache) = match ZoomClient::new(&cfg, &db, course_id).await {
        Ok(client) => match client.list_recordings(since.as_deref()).await {
            Ok(resp) => {
                db.save_meetings(course_id, &resp)?;
                (resp, false)
            }
            Err(ZoomApiError::MissingState) => {
                tracing::warn!(
                    course_id,
                    "cookies Zoom vencidas; usando datos cacheados si existen"
                );
                let cached = cached_meetings_response(&db, course_id, since.as_deref())?;
                (
                    cached.ok_or_else(|| map_api_err(ZoomApiError::MissingState))?,
                    true,
                )
            }
            Err(err) => return Err(map_api_err(err)),
        },
        Err(ZoomApiError::MissingState) => {
            tracing::warn!(course_id, "sin estado Zoom válido; se intentará con cache");
            let cached = cached_meetings_response(&db, course_id, since.as_deref())?;
            (
                cached.ok_or_else(|| map_api_err(ZoomApiError::MissingState))?,
                true,
            )
        }
        Err(err) => return Err(map_api_err(err)),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else {
        render_listing(&response);
        if from_cache {
            println!(
                "(Los datos provienen del cache local; ejecuta 'u_crawler zoom sniff-cdp' si necesitas refrescarlos.)"
            );
        }
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
        let listing = client.list_recordings(None).await.map_err(map_api_err)?;
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
    download::download_files(&cfg, &db, course_id, files, concurrency).await
}

pub async fn zoom_flow(
    course_id: u64,
    debug_port: u16,
    keep_tab: bool,
    concurrency: usize,
    since: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let mut cfg = load_config_from_path(&paths.config_file).await?;
    cfg.expand_paths();
    let db = ZoomDb::new(&paths.config_dir)?;

    sniff_cdp(SniffOptions {
        course_id,
        debug_port,
        keep_tab,
        config: &cfg,
        db: &db,
    })
    .await?;

    println!(
        "CDP completado; iniciando listado y descarga de grabaciones para el curso {}",
        course_id
    );

    let client = ZoomClient::new(&cfg, &db, course_id)
        .await
        .map_err(map_api_err)?;

    let listing = client
        .list_recordings(since.as_deref())
        .await
        .map_err(map_api_err)?;
    db.save_meetings(course_id, &listing)?;

    let meetings: Vec<RecordingSummary> = listing
        .result
        .as_ref()
        .and_then(|r| r.list.as_ref())
        .cloned()
        .unwrap_or_default();

    if meetings.is_empty() {
        println!("No se encontraron reuniones Zoom para el curso {course_id}.");
    } else {
        println!(
            "Se capturaron {} reuniones Zoom; consultando archivos individuales...",
            meetings.len()
        );
    }

    let mut all_files: Vec<ZoomRecordingFile> = Vec::new();
    for summary in meetings {
        let files = client
            .fetch_recording_files(&summary)
            .await
            .map_err(map_api_err)?;
        if files.is_empty() {
            println!(
                "- {}: sin archivos descargables reportados por Zoom",
                summary.meeting_id
            );
            continue;
        }
        db.save_files(course_id, &summary.meeting_id, &files)?;
        println!(
            "- {}: capturados {} playUrl",
            summary.meeting_id,
            files.len()
        );
        all_files.extend(files.into_iter());
    }

    if all_files.is_empty() {
        println!(
            "No hubo grabaciones con playUrl disponibles tras el flujo completo; intenta repetir el flujo o verifica permisos."
        );
        return Ok(());
    }

    capture_play_urls(CaptureOptions {
        course_id,
        debug_port,
        keep_tab,
        files: &all_files,
        db: &db,
    })
    .await?;

    download::download_files(&cfg, &db, course_id, all_files, concurrency).await
}

fn cached_meetings_response(
    db: &ZoomDb,
    course_id: u64,
    since: Option<&str>,
) -> Result<Option<RecordingListResponse>, Box<dyn Error>> {
    let meetings = db.load_meeting_payloads(course_id)?;
    if meetings.is_empty() {
        return Ok(None);
    }

    let mut list = Vec::new();
    let since_date = since.and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
    for payload in meetings {
        let summary = serde_json::from_value::<RecordingSummary>(payload)?;
        if let Some(target) = since_date {
            if let Some(start) = summary.start_time.as_deref() {
                if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(start, "%Y-%m-%d %H:%M:%S") {
                    if dt.date() < target {
                        continue;
                    }
                }
            }
        }
        list.push(summary);
    }

    if list.is_empty() {
        return Ok(None);
    }

    let total = list.len() as i64;
    let page_size = std::cmp::min(list.len(), i32::MAX as usize) as i32;

    Ok(Some(RecordingListResponse {
        status: Some(true),
        code: Some(200),
        result: Some(RecordingsResult {
            page_num: None,
            page_size: Some(page_size),
            total: Some(total),
            list: Some(list),
        }),
    }))
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
