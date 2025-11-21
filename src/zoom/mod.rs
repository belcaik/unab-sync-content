pub mod api;
pub mod db;
pub mod download;
pub mod headless;
pub mod models;

use crate::config::{load_config_from_path, ConfigPaths};
use crate::progress::progress_bar;
use api::{ZoomApiError, ZoomClient};
use db::ZoomDb;
use headless::ZoomHeadless;
use models::{RecordingSummary, ZoomRecordingFile};
use std::error::Error;
use tracing::info;

pub async fn zoom_flow(
    course_id: u64,
    concurrency: usize,
    since: Option<String>,
) -> Result<(), Box<dyn Error>> {
    let paths = ConfigPaths::default()?;
    let mut cfg = load_config_from_path(&paths.config_file).await?;
    cfg.expand_paths();
    let db = ZoomDb::new(&paths.config_dir)?;

    println!("Starting Zoom flow for course {}", course_id);

    // 1. Check if we have valid credentials (scid + cookies + headers)
    let scid = db.get_scid(course_id)?;
    let cookies = db.load_cookies()?;
    let headers = db.get_all_request_headers(course_id)?;
    
    let headless = ZoomHeadless::new(&cfg, &db, course_id);
    
    let xsrf_token = headers.iter().find(|(k, _)| k.to_lowercase() == "x-xsrf-token").map(|(_, v)| v);
    let zm_aid = headers.iter().find(|(k, _)| k.to_lowercase() == "x-zm-aid").map(|(_, v)| v);
    let zm_cluster_id = headers.iter().find(|(k, _)| k.to_lowercase() == "x-zm-cluster-id").map(|(_, v)| v);
    let zm_haid = headers.iter().find(|(k, _)| k.to_lowercase() == "x-zm-haid").map(|(_, v)| v);

    info!(
        "SESSION FROM DB -> course_id={}: lti_scid={:?}, xsrf_token={:?}, zm_aid={:?}, zm_cluster_id={:?}, zm_haid={:?}, cookies_count={}",
        course_id,
        scid,
        xsrf_token,
        zm_aid,
        zm_cluster_id,
        zm_haid,
        cookies.len(),
    );

    let has_min_creds = scid.is_some() 
        && !cookies.is_empty() 
        && xsrf_token.is_some() 
        && zm_aid.is_some() 
        && zm_cluster_id.is_some() 
        && zm_haid.is_some();

    let mut valid_session = false;

    if has_min_creds {
        println!("Found existing credentials in DB. Validating...");
        match ZoomClient::new(&cfg, &db, course_id).await {
            Ok(client) => {
                if client.validate_cookies().await {
                    println!("Cookies are valid. Skipping headless capture.");
                    valid_session = true;
                } else {
                    println!("Cookies are invalid or expired.");
                }
            }
            Err(e) => {
                println!("Failed to initialize Zoom client for validation: {}", e);
            }
        }
    } else {
        println!("Missing some credentials in DB.");
    }

    if !valid_session {
        println!("Starting headless capture (SSO + LTI scid + cookies)...");
        headless.authenticate_and_capture().await?;
        println!("Headless capture finished.");
        
        // Log what we captured
        let scid = db.get_scid(course_id)?;
        let cookies = db.load_cookies()?;
        let headers = db.get_all_request_headers(course_id)?;
        let xsrf_token = headers.iter().find(|(k, _)| k.to_lowercase() == "x-xsrf-token").map(|(_, v)| v);
        
        info!(
            "HEADLESS RESULT -> course_id={}: lti_scid={:?}, xsrf_token={:?}, cookies_count={}",
            course_id,
            scid,
            xsrf_token,
            cookies.len(),
        );
    }

    println!("Starting listing and download for course {}", course_id);

    // 2. List recordings using captured credentials
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
        println!("No Zoom meetings were found for course {course_id}.");
    } else {
        println!(
            "Captured {} Zoom meetings; fetching individual recording files...",
            meetings.len()
        );
        for meeting in &meetings {
            println!(
                "Found Meeting: ID={}, Topic='{}', Start={}",
                meeting.meeting_id,
                meeting.topic.as_deref().unwrap_or("N/A"),
                meeting.start_time.as_deref().unwrap_or("N/A")
            );
        }
    }

    // 3. Fetch recording files (API)
    let mut all_files: Vec<ZoomRecordingFile> = Vec::new();
    let meeting_progress = progress_bar(
        meetings.len() as u64,
        &format!("Gathering recording files for course {}", course_id),
    );
    for summary in meetings {
        meeting_progress.inc(1);
        meeting_progress.set_message(format!("Meeting {}", summary.meeting_id));
        let files = client
            .fetch_recording_files(&summary)
            .await
            .map_err(map_api_err)?;
        if files.is_empty() {
            meeting_progress.println(format!(
                "- {}: Zoom did not report downloadable files",
                summary.meeting_id
            ));
            continue;
        }
        db.save_files(course_id, &summary.meeting_id, &files)?;
        meeting_progress.println(format!(
            "- {}: captured {} playUrl entries",
            summary.meeting_id,
            files.len()
        ));
        all_files.extend(files.into_iter());
    }
    meeting_progress.finish_and_clear();

    if all_files.is_empty() {
        println!(
            "No recordings with playUrl entries were available after the full flow; try again or verify permissions."
        );
        return Ok(());
    }

    // 4. Capture play URLs and download immediately (one by one to avoid token expiration)
    println!("Starting capture and download (tokens expire quickly, processing one by one)...");
    headless.capture_and_download_immediately(&cfg, &db, course_id, all_files, concurrency).await?;

    println!("All recordings processed!");
    Ok(())
}

fn map_api_err(err: ZoomApiError) -> Box<dyn Error> {
    match err {
        ZoomApiError::Db(e) => e,
        other => Box::new(other),
    }
}

