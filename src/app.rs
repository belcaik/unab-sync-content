//! The top-level orchestrator: one config load and one `HttpCtx` per command,
//! one course loop, each flow in turn — so the Canvas flows no longer depend
//! on Zoom.

use crate::canvas::announcements::AnnouncementSync;
use crate::canvas::course::{course_dir, select_courses, state_path};
use crate::canvas::sync::{sync_course, SyncCounts};
use crate::canvas::CanvasClient;
use crate::core::config::Config;
use crate::core::fsutil::ensure_dir;
use crate::core::state::State;
use crate::status;
use crate::ui::progress::progress_bar;
use std::path::Path;
use tracing::{info, warn};

/// Runs the `sync` command: for every selected course, sync its modules, then
/// its announcements (when enabled), then its Zoom recordings (when enabled
/// and not a dry run).
pub async fn run_sync(
    cfg: &Config,
    filter_course_id: Option<u64>,
    dry_run: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let canvas = CanvasClient::new(cfg).await?;

    let courses = canvas.list_courses().await?;
    let selected_courses = select_courses(courses, filter_course_id, &cfg.canvas.ignored_courses);
    if selected_courses.is_empty() {
        if let Some(cid) = filter_course_id {
            tracing::warn!(
                course_id = cid,
                "course not found in active list; nothing to sync"
            );
        }
        return Ok(());
    }

    let course_progress = progress_bar(selected_courses.len() as u64, "Syncing courses");

    let mut totals = SyncCounts::default();
    for c in selected_courses {
        course_progress.inc(1);
        course_progress.set_message(format!("Syncing course {}", c.id));
        let course_dir = course_dir(Path::new(&cfg.download_root), &c);
        if !dry_run {
            ensure_dir(&course_dir).await?;
        }
        info!(course_id = c.id, path = %course_dir.display(), "sync course");

        // Load course state
        let state_path = state_path(&course_dir);
        let mut state = State::load(&state_path).await;

        totals += sync_course(&canvas, &course_dir, &c, &mut state, dry_run, verbose).await?;

        // Sync announcements for this course
        if cfg.announcements.enabled {
            match AnnouncementSync::new(cfg, &canvas, c.id, &course_dir, dry_run, verbose)
                .run(&c.name, &mut state)
                .await
            {
                Ok(s) => {
                    if dry_run && s.announcements > 0 {
                        status!(
                            "DRY-RUN announcements for course {}: {} (links: {}, media: {})",
                            c.id,
                            s.announcements,
                            s.links,
                            s.media
                        );
                    }
                }
                Err(e) => {
                    warn!(course_id = c.id, error = %e, "announcements failed for course");
                }
            }
        }

        // Sync Zoom recordings for this course.
        //
        // The Zoom flow launches a browser, performs an interactive SSO and downloads
        // video, so it must not run under --dry-run, and it must honour zoom.enabled.
        // It is only compiled in when the `zoom` feature is enabled (see AGENTS.md).
        #[cfg(feature = "zoom")]
        {
            if !cfg.zoom.enabled {
                info!(course_id = c.id, "zoom disabled in config; skipping");
            } else if dry_run {
                status!("DRY-RUN: would sync Zoom recordings for course {}", c.id);
            } else {
                status!("Starting Zoom sync for course {}...", c.id);
                match crate::zoom::zoom_flow(cfg, c.id, None).await {
                    Ok(()) => {
                        status!("✓ Zoom sync completed for course {}", c.id);
                    }
                    Err(e) => {
                        warn!(course_id = c.id, error = %e, "zoom flow failed for course");
                        // Continue with other courses even if Zoom fails
                    }
                }
            }
        }
        #[cfg(not(feature = "zoom"))]
        {
            info!(
                course_id = c.id,
                "zoom feature not compiled into this build; skipping"
            );
        }

        if !dry_run {
            state.save(&state_path).await?;
        }
    }
    course_progress.finish_and_clear();
    if dry_run {
        status!(
            "DRY-RUN summary: pages to write: {}, files to download: {}",
            totals.pages,
            totals.files
        );
    }
    Ok(())
}
