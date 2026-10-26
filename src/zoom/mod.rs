//! Zoom recordings: session capture, listing, and download into the
//! course's `grabaciones/` directory.

pub mod api;
pub mod app_conf;
pub mod db;
pub mod download;
pub mod headless;
pub mod models;
pub mod sso;

use crate::canvas::CanvasClient;
use crate::config::ConfigPaths;
use crate::layout::CourseNaming;
use crate::library;
use crate::progress::progress_bar;
use crate::state::State;
use crate::status;
use api::ZoomClient;
use db::ZoomDb;
use headless::ZoomHeadless;
use models::{RecordingSummary, ZoomRecordingFile};
use std::path::{Path, PathBuf};
use tracing::info;

/// The course a Zoom flow downloads for, and where its recordings go.
#[derive(Debug, Clone, Copy)]
pub struct ZoomTarget<'a> {
    /// Canvas course id (the Zoom session is stored per course).
    pub course_id: u64,
    /// `<download_root>/<ramo>`; recordings land in its `grabaciones/`.
    pub course_dir: &'a Path,
    /// Canvas course name, removed from recording topics.
    pub course_name: &'a str,
}

/// `zoom flow --course-id`: resolve the course's directory like `sync`
/// does, run [`zoom_flow`] and save the course state.
///
/// The course is looked up in the active list first and, failing that, in
/// the library's `curso.json` files (a course no longer active keeps its
/// recordings reachable).
///
/// # Errors
///
/// Config, Canvas or Zoom failures, or a course id neither source knows.
pub async fn run_flow(course_id: u64, since: Option<String>) -> anyhow::Result<()> {
    let cfg = crate::config::Config::load_or_init()?;
    let naming = CourseNaming::new(&cfg.layout)?;
    let root = PathBuf::from(&cfg.download_root);
    let lib = library::read(&root)?;
    let canvas = CanvasClient::from_config().await?;
    let courses = canvas.list_courses().await?;
    let dirs = library::course_dirs(&naming, &lib, &courses);

    let (dir, name) = match courses.iter().find(|c| c.id == course_id) {
        Some(course) => {
            let dir = dirs
                .get(&course_id)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no directory was named for course {course_id}"))?;
            let skeleton = library::course_index(&naming, &dir, course, lib.records.get(&dir));
            let course_dir = root.join(&dir);
            tokio::fs::create_dir_all(&course_dir).await?;
            library::claim(&course_dir, &skeleton).await?;
            (dir, course.name.clone())
        }
        None => lib
            .records
            .iter()
            .find(|(_, r)| r.canvas_id == Some(course_id))
            .map(|(dir, r)| (dir.clone(), r.name.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!("course {course_id} is neither active nor in the library")
            })?,
    };

    let course_dir = root.join(&dir);
    let state_path = library::state_path(&course_dir);
    let mut state = State::load(&state_path).await;
    let target = ZoomTarget {
        course_id,
        course_dir: &course_dir,
        course_name: &name,
    };
    let result = zoom_flow(&target, since, &mut state).await;
    // Whatever was downloaded before a failure is recorded either way.
    state.save(&state_path).await?;
    result
}

/// Capture (or reuse) the course's Zoom session, list its recordings and
/// download the new ones into `target`'s `grabaciones/`, recording each one
/// in `state` under its `zoom:` key. The caller saves `state`.
pub async fn zoom_flow(
    target: &ZoomTarget<'_>,
    since: Option<String>,
    state: &mut State,
) -> anyhow::Result<()> {
    let course_id = target.course_id;
    let cfg = crate::config::Config::load_or_init()?;
    let paths = ConfigPaths::new()?;
    let db = ZoomDb::new(&paths.config_dir)?;

    status!("Starting Zoom flow for course {}", course_id);

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
        status!("Found existing credentials in DB. Validating...");
        match ZoomClient::new(&cfg, &db, course_id).await {
            Ok(client) if client.validate_cookies().await => {
                status!("Cookies are valid. Skipping headless capture.");
                valid_session = true;
            }
            Ok(_) => status!("Cookies are invalid or expired."),
            Err(e) => status!("Failed to initialize Zoom client for validation: {}", e),
        }
    } else {
        status!("No complete session stored for this course.");
    }

    if !valid_session {
        status!("Starting headless capture (SSO + LTI scid + cookies)...");
        headless.authenticate_and_capture().await?;
        status!("Headless capture finished.");

        info!(
            course_id,
            captured = db.load_session(course_id)?.is_some(),
            "headless capture result"
        );
    }

    status!("Starting listing and download for course {}", course_id);

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
        status!("No Zoom meetings were found for course {course_id}.");
    } else {
        status!(
            "Captured {} Zoom meetings; fetching individual recording files...",
            meetings.len()
        );
        for meeting in &meetings {
            status!(
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
        all_files.extend(files);
    }
    meeting_progress.finish_and_clear();

    if all_files.is_empty() {
        status!("No recordings with playUrl entries were available after the full flow; try again or verify permissions.");
        return Ok(());
    }

    // 4. Capture play URLs and download immediately (one by one to avoid token expiration)
    status!("Starting capture and download (tokens expire quickly, processing one by one)...");
    headless
        .capture_and_download_immediately(all_files, target, state)
        .await?;

    status!("All recordings processed!");
    Ok(())
}
