pub mod api;
pub mod app_conf;
pub mod db;
pub mod download;
pub mod headless;
pub mod models;
pub mod sso;

use crate::config::ConfigPaths;
use crate::progress::progress_bar;
use api::ZoomClient;
use db::ZoomDb;
use headless::ZoomHeadless;
use models::{RecordingSummary, ZoomRecordingFile};
use tracing::info;

pub async fn zoom_flow(course_id: u64, since: Option<String>) -> anyhow::Result<()> {
    let cfg = crate::config::Config::load_or_init()?;
    let paths = ConfigPaths::new()?;
    let db = ZoomDb::new(&paths.config_dir)?;

    println!("Starting Zoom flow for course {}", course_id);

    // 1. Reuse the stored session if it is complete and still accepted.
    let headless = ZoomHeadless::new(&cfg, &db, course_id);
    let stored = db.load_session(course_id)?;

    info!(
        course_id,
        has_session = stored.is_some(),
        "checked stored zoom session"
    );

    let mut valid_session = false;
    if stored.is_some() {
        println!("Found existing credentials in DB. Validating...");
        match ZoomClient::new(&cfg, &db, course_id).await {
            Ok(client) if client.validate_cookies().await => {
                println!("Cookies are valid. Skipping headless capture.");
                valid_session = true;
            }
            Ok(_) => println!("Cookies are invalid or expired."),
            Err(e) => println!("Failed to initialize Zoom client for validation: {}", e),
        }
    } else {
        println!("No complete session stored for this course.");
    }

    if !valid_session {
        println!("Starting headless capture (SSO + LTI scid + cookies)...");
        headless.authenticate_and_capture().await?;
        println!("Headless capture finished.");

        info!(
            course_id,
            captured = db.load_session(course_id)?.is_some(),
            "headless capture result"
        );
    }

    println!("Starting listing and download for course {}", course_id);

    // 2. List recordings using captured credentials
    let client = ZoomClient::new(&cfg, &db, course_id).await?;

    let listing = client.list_recordings(since.as_deref()).await?;
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
        let files = client.fetch_recording_files(&summary).await?;
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
    headless.capture_and_download_immediately(all_files).await?;

    println!("All recordings processed!");
    Ok(())
}
