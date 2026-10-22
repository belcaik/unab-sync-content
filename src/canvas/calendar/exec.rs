//! The I/O executor half of the calendar-sync flow (spec D10): fetch, plan,
//! apply, record — with per-course resilience (spec ticket 11).

use std::path::PathBuf;

use chrono::Utc;

use super::plan::{content_hash, plan_for_course};
use super::{Plan, PlannedDelete, PlannedWrite};
use crate::canvas::course::course_dir;
use crate::canvas::{Assignment, CanvasClient, Course, Submission};
use crate::core::config::Config;
use crate::core::fsutil;
use crate::core::state::{ItemState, State};
use crate::status;

/// Select which courses the calendar-sync executor should plan for.
///
/// Pure selection logic, extracted so it is testable without a network call:
/// the executor (`run_calendar` in `main.rs`) is the only caller, and mirrors
/// the same course/`ignored_courses` interplay already used by
/// `announcements::run_discovery`. When `filter_course_id` is set it wins
/// outright — an explicit `--course-id` is an instruction to plan that one
/// course, ignored or not. Otherwise, courses listed in `ignored_courses`
/// (matched by their string id, as `canvas.ignored_courses` stores them) are
/// dropped: they produce no calendars (spec user story 15).
pub fn select_active_courses(
    courses: Vec<Course>,
    filter_course_id: Option<u64>,
    ignored_courses: &[String],
) -> Vec<Course> {
    if let Some(cid) = filter_course_id {
        return courses.into_iter().filter(|c| c.id == cid).collect();
    }
    courses
        .into_iter()
        .filter(|c| !ignored_courses.iter().any(|id| id == &c.id.to_string()))
        .collect()
}

/// The outcome of syncing one course's assignments (spec ticket 11): either
/// it produced a plan, or fetching its assignments failed and it is skipped
/// so the other courses can still sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CourseResult {
    Synced { writes: usize, deletes: usize },
    Failed,
}

/// What one run of the calendar-sync flow produced, folded from each
/// course's [`CourseResult`]. Pure — no network, no disk — so the fold and
/// the verdict it feeds ([`conclude`]) are directly testable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunSummary {
    pub synced: usize,
    pub failed: usize,
    pub writes: usize,
    pub deletes: usize,
}

impl RunSummary {
    /// Fold per-course results into totals.
    pub fn from_results(results: &[CourseResult]) -> Self {
        let mut summary = RunSummary::default();
        for result in results {
            match result {
                CourseResult::Synced { writes, deletes } => {
                    summary.synced += 1;
                    summary.writes += writes;
                    summary.deletes += deletes;
                }
                CourseResult::Failed => summary.failed += 1,
            }
        }
        summary
    }
}

/// The overall verdict of a calendar-sync run, once every selected course has
/// been attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// Every selected course synced.
    Success,
    /// At least one course synced and at least one failed. The run produced
    /// something real but cron should still be told (exit code 13 — see
    /// `AGENTS.md`).
    PartialFailure,
}

/// Every selected course failed to sync: the run accomplished nothing, so it
/// must be reported as a hard failure rather than a partial one.
#[derive(Debug, thiserror::Error)]
#[error("all {failed} course(s) failed to sync; see logs for per-course errors")]
pub struct TotalFailure {
    pub failed: usize,
}

/// Decide the run's verdict from a folded [`RunSummary`]. Pure: no network,
/// no disk, callable with hand-built summaries in tests (spec ticket 11).
pub fn conclude(summary: RunSummary) -> Result<RunOutcome, TotalFailure> {
    match (summary.synced, summary.failed) {
        (_, 0) => Ok(RunOutcome::Success),
        (0, failed) => Err(TotalFailure { failed }),
        _ => Ok(RunOutcome::PartialFailure),
    }
}

/// Runs the calendar-sync flow end to end (spec D10, the executor half).
///
/// Fetches active courses (honouring `canvas.ignored_courses` via
/// [`select_active_courses`], and `filter_course_id` when given), fetches each
/// course's assignments through [`CanvasClient::list_assignments`] (which goes
/// through the shared paginator), calls the pure [`plan`](fn@super::plan::plan), and applies the
/// result with [`fsutil::atomic_write`].
///
/// Each course's submissions are fetched with one bulk call
/// ([`CanvasClient::list_submissions`], spec D1, ticket 09) rather than one
/// per assignment, and handed to [`plan`](fn@super::plan::plan) alongside that course's
/// assignments. A submissions fetch failure is treated the same as an
/// assignments fetch failure (spec ticket 11): the course is logged and
/// skipped rather than aborting the run. The previous
/// [`State`] *is* wired in (ticket 06): it is loaded from the course's
/// existing `state.json` under `download_root` — the same file `sync` and
/// `announcements` already read and write, under the `calendar:{id}`
/// namespace (spec D5) — never from anywhere inside `caldir_root`. After a
/// successful apply, every written assignment's content hash is recorded
/// back into that state and saved, so the next run's [`plan`](fn@super::plan::plan) call has
/// something to compare against. In `--dry-run` nothing is persisted: state
/// is loaded (to plan correctly) but never saved.
///
/// `dry_run` reports the plan without writing a byte and without creating any
/// directory — matching the `--dry-run` contract the rest of the CLI upholds.
///
/// Per spec ticket 11 (resiliencia por ramo), a course that fails does
/// **not** abort the run — whether the failure happens fetching its
/// assignments or writing/deleting its calendar files. Either way it is
/// logged with its course id and counted as failed via
/// [`CourseResult::Failed`], and the remaining courses still sync. A course
/// that fails partway through its writes is counted as failed as a whole;
/// nothing already written is rolled back, because each file lands atomically
/// ([`fsutil::atomic_write`]) and a partial course self-heals on the next run.
/// A `NotFound` on delete is not a failure at all — the file is already gone,
/// which is the desired end state. The fold of every course's
/// [`CourseResult`] into a [`RunSummary`] and the verdict derived from it
/// ([`conclude`]) are pure and covered by unit tests; only the network/disk
/// plumbing around them is exercised here. The return type widens from
/// `anyhow::Result<()>` to `anyhow::Result<RunOutcome>` so `main.rs` can tell
/// a full success apart from a partial one (exit code 13) — a hard error
/// (config, auth, or every course failing) still comes back as `Err`,
/// unchanged from before.
///
/// **Deletion (spec D5, ticket 10).** [`plan`](fn@super::plan::plan) compares `assignments`
/// against `state` and plans a delete for any component whose assignment
/// Canvas no longer mentions. [`plan_for_course`](super::plan::plan_for_course) is the gate that decides
/// whether `plan` runs at all: assignments are only unwrapped to a bare
/// `Vec` (losing the fetch-failed/fetch-empty distinction) *after* both
/// fetches are confirmed `Ok`, so a failed fetch can never reach `plan` and
/// be mistaken for "this course now has no assignments". On a successful
/// apply, [`record_deletes`] removes the deleted components' entries from
/// `state` — the mirror of [`record_writes`] — so a deleted assignment is
/// not deleted again, forever, on every subsequent run. In `--dry-run`,
/// deletes are reported (see the `deletes` field logged below) and nothing
/// is removed, on disk or in `state`, exactly like writes.
pub async fn run_calendar(
    filter_course_id: Option<u64>,
    dry_run: bool,
) -> anyhow::Result<RunOutcome> {
    let cfg = Config::load_or_init()?;

    if !cfg.calendar.enabled {
        status!("Calendar sync is disabled (calendar.enabled = false).");
        return Ok(RunOutcome::Success);
    }

    let caldir_root = PathBuf::from(&cfg.calendar.caldir_root);
    let download_root = PathBuf::from(&cfg.download_root);
    let canvas = CanvasClient::from_config().await?;

    let courses = canvas.list_courses().await?;
    let selected = select_active_courses(courses, filter_course_id, &cfg.canvas.ignored_courses);

    if selected.is_empty() {
        status!("No matching courses.");
        return Ok(RunOutcome::Success);
    }

    let now = Utc::now();
    let mut results = Vec::with_capacity(selected.len());

    for course in &selected {
        let assignments: Result<Vec<Assignment>, ()> =
            match canvas.list_assignments(course.id).await {
                Ok(assignments) => Ok(assignments),
                Err(e) => {
                    tracing::error!(
                        course_id = course.id,
                        error = %e,
                        "failed to fetch assignments; skipping this course"
                    );
                    Err(())
                }
            };

        // Only attempt the submissions fetch when the assignments fetch
        // already succeeded — no point spending a second call on a course
        // that is going to be skipped regardless.
        let submissions: Result<Vec<Submission>, ()> = if assignments.is_ok() {
            match canvas.list_submissions(course.id).await {
                Ok(submissions) => Ok(submissions),
                Err(e) => {
                    tracing::error!(
                        course_id = course.id,
                        error = %e,
                        "failed to fetch submissions; skipping this course"
                    );
                    Err(())
                }
            }
        } else {
            Err(())
        };

        let state_path = course_dir(&download_root, course).join("state.json");
        let mut state = State::load(&state_path).await;

        // Ticket 10's safety requirement: a fetch failure must never reach
        // `plan` disguised as an empty `Vec` — see `plan_for_course`'s doc
        // comment. `None` here means "no plan attempted", not "empty plan".
        let Some(course_plan) =
            plan_for_course(&caldir_root, course, assignments, submissions, now, &state)
        else {
            results.push(CourseResult::Failed);
            continue;
        };

        if dry_run {
            tracing::info!(
                course_id = course.id,
                writes = course_plan.writes.len(),
                deletes = course_plan.deletes.len(),
                "dry-run calendar plan"
            );
            results.push(CourseResult::Synced {
                writes: course_plan.writes.len(),
                deletes: course_plan.deletes.len(),
            });
            continue;
        }

        match apply_plan(course.id, &course_plan).await {
            Ok(()) => {
                record_writes(&mut state, &course_plan.writes);
                record_deletes(&mut state, &course_plan.deletes);
                if let Err(e) = state.save(&state_path).await {
                    tracing::error!(
                        course_id = course.id,
                        error = %e,
                        "failed to persist calendar state; skipping the rest of this course"
                    );
                    results.push(CourseResult::Failed);
                    continue;
                }
                results.push(CourseResult::Synced {
                    writes: course_plan.writes.len(),
                    deletes: course_plan.deletes.len(),
                });
            }
            Err(e) => {
                tracing::error!(
                    course_id = course.id,
                    error = %e,
                    "failed to write this course's calendar files; skipping the rest of this course"
                );
                results.push(CourseResult::Failed);
            }
        }
    }

    let summary = RunSummary::from_results(&results);

    status!(
        "{}Calendar: {} deadline file(s) written, {} removed, across {} course(s) synced, {} failed",
        if dry_run { "DRY-RUN: " } else { "" },
        summary.writes,
        summary.deletes,
        summary.synced,
        summary.failed,
    );

    conclude(summary).map_err(anyhow::Error::from)
}

/// Apply one course's plan to disk: write every planned file, then remove
/// every planned deletion. Isolated from `run_calendar` so a failure here is
/// caught and turned into `CourseResult::Failed` instead of aborting the
/// whole run (spec ticket 11) — a bad-permissions volume mount under cron
/// must not cost every other course its update.
///
/// A `NotFound` on delete is not an error: the file is already gone, which is
/// the wanted end state.
async fn apply_plan(course_id: u64, course_plan: &Plan) -> std::io::Result<()> {
    for write in &course_plan.writes {
        fsutil::atomic_write(&write.path, write.content.as_bytes()).await?;
        tracing::info!(course_id, path = %write.path.display(), "wrote calendar file");
    }
    for delete in &course_plan.deletes {
        match tokio::fs::remove_file(&delete.path).await {
            Ok(()) => {
                tracing::info!(course_id, path = %delete.path.display(), "removed calendar file")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Record every successfully-applied write's content hash back into `state`,
/// under its own `state_key` (deadline or window — see [`PlannedWrite`]), so
/// the next run's [`plan`](fn@super::plan::plan) call sees what was actually written and can
/// produce an empty plan when Canvas hasn't changed. Does not save to disk —
/// the caller does that, and only outside `--dry-run`.
pub(super) fn record_writes(state: &mut State, writes: &[PlannedWrite]) {
    for write in writes {
        let hash = content_hash(&write.content);
        state.set(
            write.state_key.clone(),
            ItemState {
                etag: None,
                updated_at: None,
                size: Some(write.content.len() as u64),
                content_hash: Some(hash),
                last_error: None,
                error_count: None,
            },
        );
    }
}

/// Remove every successfully-applied delete's entry from `state` (spec D5,
/// ticket 10), the mirror of [`record_writes`]. Without this, an assignment
/// removed from Canvas would look "deleted" again on every subsequent run
/// forever: `plan` would keep finding its stale `calendar:{id}` /
/// `calendar-window:{id}` entry in `prev`, plan another (harmless but
/// pointless) delete of an already-missing file, and never let the state
/// entry go. Does not save to disk — the caller does that, and only outside
/// `--dry-run`.
pub(super) fn record_deletes(state: &mut State, deletes: &[PlannedDelete]) {
    for delete in deletes {
        state.items.remove(&delete.state_key);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_plan, conclude, record_deletes, record_writes, select_active_courses, CourseResult,
        RunOutcome, RunSummary,
    };
    use crate::canvas::calendar::identity::calendar_state_key;
    use crate::canvas::calendar::plan::plan;
    use crate::canvas::calendar::test_support::{
        assignment_with_due_date, course, course_with, fixed_now,
    };
    use crate::canvas::calendar::{Plan, PlannedDelete, PlannedWrite};
    use crate::core::state::{ItemState, State};
    use std::path::{Path, PathBuf};

    #[test]
    fn record_writes_stores_the_content_hash_under_the_calendar_namespace() {
        let mut state = State::default();
        let writes = vec![PlannedWrite {
            path: PathBuf::from("/caldir/course/deadlines/assignment-9.ics"),
            content: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
            assignment_id: 9,
            state_key: calendar_state_key(9),
        }];

        record_writes(&mut state, &writes);

        let stored = state.get("calendar:9").expect("key recorded");
        // A known-good literal, not `content_hash(...)` recomputed: asserting
        // against the helper under test could never disagree with it.
        assert_eq!(
            stored.content_hash.as_deref(),
            Some("8b506967e3fe6f7878a37106c9c3ff102684e991")
        );
    }

    #[test]
    fn a_second_plan_against_the_recorded_state_is_empty() {
        // End-to-end of the executor contract: plan, record what was
        // written, plan again against that recorded state -> nothing.
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let first = plan(
            root,
            &course(),
            std::slice::from_ref(&assignment),
            &[],
            fixed_now(),
            &State::default(),
        );
        let mut state = State::default();
        record_writes(&mut state, &first.writes);

        let second = plan(root, &course(), &[assignment], &[], fixed_now(), &state);

        assert!(second.writes.is_empty());
    }

    #[test]
    fn ignored_courses_produce_no_calendars() {
        let courses = vec![
            course_with(1, "Kept"),
            course_with(2, "Ignored"),
            course_with(3, "Also Kept"),
        ];
        let got = select_active_courses(courses, None, &["2".to_string()]);
        let ids: Vec<u64> = got.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn no_ignored_courses_keeps_everything() {
        let courses = vec![course_with(1, "A"), course_with(2, "B")];
        let got = select_active_courses(courses, None, &[]);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn filter_course_id_wins_over_ignored_courses() {
        let courses = vec![course_with(1, "A"), course_with(2, "B")];
        // Course 2 is ignored, but an explicit --course-id=2 must still select it.
        let got = select_active_courses(courses, Some(2), &["2".to_string()]);
        let ids: Vec<u64> = got.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn filter_course_id_excludes_every_other_course() {
        let courses = vec![
            course_with(1, "A"),
            course_with(2, "B"),
            course_with(3, "C"),
        ];
        let got = select_active_courses(courses, Some(2), &[]);
        let ids: Vec<u64> = got.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![2]);
    }

    // --- per-course resilience (ticket 11): pure fold of per-course results
    // into a summary, and the verdict derived from it. No network involved. ---

    #[test]
    fn summary_counts_synced_courses_and_their_writes() {
        let results = vec![
            CourseResult::Synced {
                writes: 3,
                deletes: 0,
            },
            CourseResult::Synced {
                writes: 2,
                deletes: 1,
            },
        ];
        let summary = RunSummary::from_results(&results);
        assert_eq!(summary.synced, 2);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.writes, 5);
        assert_eq!(summary.deletes, 1);
    }

    #[test]
    fn summary_counts_failed_courses_separately_from_synced() {
        let results = vec![
            CourseResult::Synced {
                writes: 1,
                deletes: 0,
            },
            CourseResult::Failed,
            CourseResult::Synced {
                writes: 4,
                deletes: 2,
            },
        ];
        let summary = RunSummary::from_results(&results);
        assert_eq!(summary.synced, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.writes, 5);
        assert_eq!(summary.deletes, 2);
    }

    #[test]
    fn all_courses_synced_concludes_success() {
        let summary = RunSummary {
            synced: 3,
            failed: 0,
            writes: 7,
            deletes: 0,
        };
        assert_eq!(conclude(summary).unwrap(), RunOutcome::Success);
    }

    #[test]
    fn some_courses_failed_concludes_partial_failure() {
        let summary = RunSummary {
            synced: 2,
            failed: 1,
            writes: 4,
            deletes: 0,
        };
        assert_eq!(conclude(summary).unwrap(), RunOutcome::PartialFailure);
    }

    #[test]
    fn all_courses_failed_concludes_total_failure() {
        let summary = RunSummary {
            synced: 0,
            failed: 3,
            writes: 0,
            deletes: 0,
        };
        let err = conclude(summary).unwrap_err();
        assert_eq!(err.failed, 3);
    }

    // --- apply_plan (ticket 11 follow-up): a course's write/delete failure
    // must be classified the same as a fetch failure. apply_plan is the I/O
    // executor half (spec "Fuera del alcance de los tests" keeps run_calendar
    // itself untested), so these two exercise real disk I/O against a
    // tempdir rather than a mock -- the repo has no filesystem-mocking
    // infrastructure and this ticket does not add one. What they pin is
    // narrow: does apply_plan surface an `Err` (which run_calendar's match
    // arm turns into `CourseResult::Failed`) on a genuine write failure, and
    // does it stay `Ok` (=> `CourseResult::Synced`) when a delete target is
    // simply already gone.

    #[tokio::test]
    async fn apply_plan_reports_a_write_failure_instead_of_succeeding() {
        let tmp = tempfile::tempdir().unwrap();
        // Put a plain file where a directory needs to go, so atomic_write's
        // create_dir_all hits a real ENOTDIR -- not a simulated error.
        let blocker = tmp.path().join("blocked");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let path = blocker.join("assignment-1.ics");
        let course_plan = Plan {
            writes: vec![PlannedWrite {
                path,
                content: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
                assignment_id: 1,
                state_key: calendar_state_key(1),
            }],
            deletes: Vec::new(),
        };

        let result = apply_plan(1, &course_plan).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn apply_plan_treats_an_already_missing_delete_target_as_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does-not-exist.ics");
        let course_plan = Plan {
            writes: Vec::new(),
            deletes: vec![PlannedDelete {
                path,
                assignment_id: 1,
                state_key: calendar_state_key(1),
            }],
        };

        let result = apply_plan(1, &course_plan).await;

        assert!(result.is_ok());
    }

    #[test]
    fn record_deletes_removes_the_state_entry() {
        let mut state = State::default();
        state.set(
            calendar_state_key(9),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("whatever".into()),
                last_error: None,
                error_count: None,
            },
        );
        let deletes = vec![PlannedDelete {
            path: PathBuf::from("/caldir/course/deadlines/assignment-9.ics"),
            assignment_id: 9,
            state_key: calendar_state_key(9),
        }];

        record_deletes(&mut state, &deletes);

        assert!(state.get(&calendar_state_key(9)).is_none());
    }

    #[test]
    fn deleting_one_assignment_does_not_disturb_a_kept_ones_state_entry() {
        let mut state = State::default();
        state.set(
            calendar_state_key(1),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("kept".into()),
                last_error: None,
                error_count: None,
            },
        );
        state.set(
            calendar_state_key(2),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("gone".into()),
                last_error: None,
                error_count: None,
            },
        );
        let deletes = vec![PlannedDelete {
            path: PathBuf::from("/caldir/course/deadlines/assignment-2.ics"),
            assignment_id: 2,
            state_key: calendar_state_key(2),
        }];

        record_deletes(&mut state, &deletes);

        assert!(state.get(&calendar_state_key(1)).is_some());
        assert!(state.get(&calendar_state_key(2)).is_none());
    }
}
