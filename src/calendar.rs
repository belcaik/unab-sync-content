//! Pure planning of deadline calendar files for the calendar-sync flow, plus
//! the I/O executor that runs it end to end.
//!
//! [`plan`] decides which calendar files should exist for a course's
//! assignment deadlines. It performs no I/O and reads no clock — the current
//! instant is injected by the caller. See `docs/specs/calendar-sync-flow.md`
//! (D3, D4, D9, D10) for the design this implements. This module only
//! understands deadlines (`VTODO`); priority, availability windows and
//! submission status are later work that widens the same seam.
//!
//! [`run_calendar`] is the executor half (spec D10): it fetches active
//! courses and their assignments, calls [`plan`], and applies the result to
//! disk with [`fsutil::atomic_write`]. Per spec "Fuera del alcance de los
//! tests", the executor and the Canvas calls are untested here — the repo has
//! no HTTP mock server and this ticket does not add one. [`select_active_courses`]
//! is pulled out of the executor specifically so the one piece of decision
//! logic in it (course/`ignored_courses` selection) stays testable without a
//! network.
//!
//! Per-course resilience (spec ticket 11) has its own pure seam: a course
//! whose assignment fetch fails is recorded as [`CourseResult::Failed`]
//! instead of aborting the run, [`RunSummary::from_results`] folds every
//! course's result into totals, and [`conclude`] turns that fold into a
//! [`RunOutcome`] (or a [`TotalFailure`] when every course failed). All three
//! are plain data in, data out — testable without touching the network.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::canvas::{Assignment, CanvasClient, Course};
use crate::config::Config;
use crate::fsutil;
use crate::state::State;
use crate::status;

/// A submission record for one assignment.
///
/// Intentionally minimal: this ticket accepts submissions as a parameter so
/// later tickets (submission/completed status) can widen [`plan`] without
/// changing call sites, but does not yet act on submission data.
#[derive(Debug, Clone)]
pub struct Submission {
    pub assignment_id: u64,
}

/// One calendar file to write, in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWrite {
    pub path: PathBuf,
    pub content: String,
}

/// The outcome of planning: which files to write and which to delete.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub writes: Vec<PlannedWrite>,
    pub deletes: Vec<PathBuf>,
}

/// The on-disk name of the directory holding deadline `VTODO` files for a
/// course. A sibling directory for a future "availability window" semantics
/// (spec D4) can be added later without moving anything under this one.
const DEADLINES_DIR: &str = "deadlines";

/// Derive the calendar UID for an assignment's deadline `VTODO`.
///
/// Derived only from the Canvas assignment id, so it survives title and due
/// date changes — the same id also names the on-disk file (via
/// [`deadline_filename`]), which is why a moved deadline rewrites the same
/// path instead of orphaning the old one (see the spike findings in
/// `docs/specs/calendar-sync-flow.md`).
///
/// The `todo-` segment is a semantic discriminator, not decoration: UIDs are
/// the server-side identity of a published CalDAV object and must be
/// globally unique across the account. Ticket 08 adds a `VEVENT` for the
/// same assignment id under `u_crawler-window-{id}@…` — without the
/// discriminator the two would collide, which is exactly the VTODO/VEVENT
/// confusion the spec cites in Radicale issue #101. Changing this scheme
/// after the first `caldir push` would orphan already-published objects, so
/// it is fixed now rather than in ticket 08.
fn deadline_uid(assignment_id: u64) -> String {
    format!("u_crawler-todo-{assignment_id}@u-crawler.local")
}

/// Derive the on-disk filename for an assignment's deadline `VTODO`. Shares
/// its source (the assignment id) with [`deadline_uid`], never the due date.
fn deadline_filename(assignment_id: u64) -> String {
    format!("assignment-{assignment_id}.ics")
}

/// Format a UTC instant the way iCalendar wants it: `Z`-suffixed, no offset
/// arithmetic required by the reader (spec D9).
fn ics_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Escape iCalendar TEXT value special characters (RFC 5545 §3.3.11).
fn escape_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

/// Derive the `DTSTAMP` for a deadline `VTODO` (RFC 5545 §3.6.2 requires
/// exactly one). This must NOT be `now`: doing so would make every run emit
/// different content for unchanged data, defeating the "no changes, empty
/// plan" guarantee later tickets build on (spec user story 14). Instead it is
/// derived from data that only changes when Canvas's own record changes:
/// `assignment.updated_at` when present and RFC 3339, falling back to
/// `due_at` — both stable across repeated runs over the same input.
fn dtstamp_for(assignment: &Assignment, due: DateTime<Utc>) -> DateTime<Utc> {
    assignment
        .updated_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(due)
}

/// Render a single assignment deadline as a complete `.ics` file: one
/// `VCALENDAR` wrapping one `VTODO`.
fn render_vtodo(uid: &str, assignment: &Assignment, due: DateTime<Utc>) -> String {
    let dtstamp = dtstamp_for(assignment, due);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//u_crawler//calendar-sync//EN".to_string(),
        "BEGIN:VTODO".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(dtstamp)),
        format!("DUE:{}", ics_datetime(due)),
        format!(
            "SUMMARY:{}",
            escape_text(assignment.name.as_deref().unwrap_or(""))
        ),
    ];
    if let Some(url) = &assignment.html_url {
        lines.push(format!("URL:{}", escape_text(url)));
    }
    lines.push("END:VTODO".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n") + "\r\n"
}

/// Plan the deadline calendar files for one course.
///
/// Pure: no network, no disk access and no system clock — `now` is the
/// caller's injected instant. `submissions` and `prev` are accepted so later
/// tickets can widen this function without changing every call site; this
/// ticket does not build logic on them yet.
pub fn plan(
    caldir_root: &Path,
    course: &Course,
    assignments: &[Assignment],
    _submissions: &[Submission],
    _now: DateTime<Utc>,
    _prev: &State,
) -> Plan {
    let dir = fsutil::course_dir(caldir_root, course).join(DEADLINES_DIR);
    let mut writes = Vec::new();
    for assignment in assignments {
        let Some(due) = assignment.due_at else {
            continue;
        };
        let uid = deadline_uid(assignment.id);
        let path = dir.join(deadline_filename(assignment.id));
        let content = render_vtodo(&uid, assignment, due);
        writes.push(PlannedWrite { path, content });
    }
    Plan {
        writes,
        deletes: Vec::new(),
    }
}

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
    Synced { writes: usize },
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
}

impl RunSummary {
    /// Fold per-course results into totals.
    pub fn from_results(results: &[CourseResult]) -> Self {
        let mut summary = RunSummary::default();
        for result in results {
            match result {
                CourseResult::Synced { writes } => {
                    summary.synced += 1;
                    summary.writes += writes;
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
/// through the shared paginator), calls the pure [`plan`], and applies the
/// result with [`fsutil::atomic_write`].
///
/// Submission data and the previous [`State`] are not wired in yet: submission
/// status is ticket 09's scope, and `state.json` diffing for the calendar
/// namespace is ticket 06's — this ticket always plans against an empty
/// submission list and a fresh, empty `State`, which is what makes every
/// matching assignment with a due date always get (re)written.
///
/// `dry_run` reports the plan without writing a byte and without creating any
/// directory — matching the `--dry-run` contract the rest of the CLI upholds.
///
/// Per spec ticket 11 (resiliencia por ramo), a course whose assignment fetch
/// fails does **not** abort the run: it is logged with its course id and
/// counted as failed via [`CourseResult::Failed`], and the remaining courses
/// still sync. The fold of every course's [`CourseResult`] into a
/// [`RunSummary`] and the verdict derived from it ([`conclude`]) are pure and
/// covered by unit tests; only the network/disk plumbing around them is
/// exercised here. The return type widens from `anyhow::Result<()>` to
/// `anyhow::Result<RunOutcome>` so `main.rs` can tell a full success apart
/// from a partial one (exit code 13) — a hard error (config, auth, or every
/// course failing) still comes back as `Err`, unchanged from before.
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
        let assignments = match canvas.list_assignments(course.id).await {
            Ok(assignments) => assignments,
            Err(e) => {
                tracing::error!(
                    course_id = course.id,
                    error = %e,
                    "failed to fetch assignments; skipping this course"
                );
                results.push(CourseResult::Failed);
                continue;
            }
        };
        let course_plan = plan(
            &caldir_root,
            course,
            &assignments,
            &[],
            now,
            &State::default(),
        );

        if dry_run {
            tracing::info!(
                course_id = course.id,
                writes = course_plan.writes.len(),
                deletes = course_plan.deletes.len(),
                "dry-run calendar plan"
            );
            results.push(CourseResult::Synced {
                writes: course_plan.writes.len(),
            });
            continue;
        }

        for write in &course_plan.writes {
            fsutil::atomic_write(&write.path, write.content.as_bytes()).await?;
            tracing::info!(course_id = course.id, path = %write.path.display(), "wrote calendar file");
        }
        for delete in &course_plan.deletes {
            match tokio::fs::remove_file(delete).await {
                Ok(()) => {
                    tracing::info!(course_id = course.id, path = %delete.display(), "removed calendar file")
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }

        results.push(CourseResult::Synced {
            writes: course_plan.writes.len(),
        });
    }

    let summary = RunSummary::from_results(&results);

    status!(
        "{}Calendar: {} deadline file(s) across {} course(s) synced, {} failed",
        if dry_run { "DRY-RUN: " } else { "" },
        summary.writes,
        summary.synced,
        summary.failed,
    );

    conclude(summary).map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Course;

    /// A fixed instant, so a future accidental dependency on `now` in `plan`
    /// shows up as a test failure rather than as flake.
    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-23T00:00:00Z")
            .unwrap()
            .into()
    }

    fn course() -> Course {
        Course {
            id: 1,
            name: "Intro to Testing".into(),
            course_code: Some("TST101".into()),
        }
    }

    #[test]
    fn empty_assignment_set_produces_an_empty_plan() {
        let root = Path::new("/caldir");
        let got = plan(root, &course(), &[], &[], fixed_now(), &State::default());
        assert!(got.writes.is_empty());
        assert!(got.deletes.is_empty());
    }

    fn assignment_without_due_date() -> Assignment {
        Assignment {
            id: 42,
            name: Some("Reading Reflection".into()),
            description: None,
            updated_at: None,
            due_at: None,
            unlock_at: None,
            lock_at: None,
            points_possible: None,
            omit_from_final_grade: None,
            html_url: None,
            assignment_group_id: None,
            submission_types: None,
            published: None,
        }
    }

    #[test]
    fn assignment_without_due_date_produces_no_components() {
        let root = Path::new("/caldir");
        let assignments = vec![assignment_without_due_date()];
        let got = plan(
            root,
            &course(),
            &assignments,
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes.is_empty());
        assert!(got.deletes.is_empty());
    }

    fn assignment_with_due_date() -> Assignment {
        Assignment {
            id: 555,
            name: Some("Essay Draft".into()),
            description: None,
            updated_at: None,
            due_at: Some(
                DateTime::parse_from_rfc3339("2026-09-01T23:59:00Z")
                    .unwrap()
                    .into(),
            ),
            unlock_at: None,
            lock_at: None,
            points_possible: None,
            omit_from_final_grade: None,
            html_url: Some("https://canvas.example.edu/courses/1/assignments/555".into()),
            assignment_group_id: None,
            submission_types: None,
            published: None,
        }
    }

    #[test]
    fn assignment_with_due_date_produces_a_vtodo_write() {
        let root = Path::new("/caldir");
        let assignments = vec![assignment_with_due_date()];
        let got = plan(
            root,
            &course(),
            &assignments,
            &[],
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.deletes, Vec::<PathBuf>::new());
        assert_eq!(got.writes.len(), 1);
        let write = &got.writes[0];
        assert_eq!(
            write.path,
            Path::new("/caldir/Intro_to_Testing_TST101/deadlines/assignment-555.ics")
        );
        assert!(write.content.contains("BEGIN:VTODO"));
        assert!(write
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert!(write.content.contains("DUE:20260901T235900Z"));
        assert!(write.content.contains("SUMMARY:Essay Draft"));
        assert!(write
            .content
            .contains("URL:https://canvas.example.edu/courses/1/assignments/555"));
        assert!(write.content.contains("END:VTODO"));
    }

    #[test]
    fn same_assignment_id_keeps_the_same_uid_and_path_across_title_and_date_changes() {
        let root = Path::new("/caldir");
        let original = assignment_with_due_date();
        let renamed_and_rescheduled = Assignment {
            id: 555,
            name: Some("Essay Final".into()),
            description: None,
            updated_at: None,
            due_at: Some(
                DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
                    .unwrap()
                    .into(),
            ),
            unlock_at: None,
            lock_at: None,
            points_possible: None,
            omit_from_final_grade: None,
            html_url: Some("https://canvas.example.edu/courses/1/assignments/555".into()),
            assignment_group_id: None,
            submission_types: None,
            published: None,
        };

        let before = plan(
            root,
            &course(),
            &[original],
            &[],
            fixed_now(),
            &State::default(),
        );
        let after = plan(
            root,
            &course(),
            &[renamed_and_rescheduled],
            &[],
            fixed_now(),
            &State::default(),
        );

        assert_eq!(before.writes[0].path, after.writes[0].path);
        assert!(after.writes[0]
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert!(after.writes[0].content.contains("DUE:20261015T120000Z"));
    }

    #[test]
    fn dtstamp_is_derived_from_updated_at_when_present_and_parseable() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.updated_at = Some("2026-08-20T10:15:00Z".into());
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("DTSTAMP:20260820T101500Z"));
    }

    #[test]
    fn dtstamp_falls_back_to_due_at_when_updated_at_is_absent() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        assert!(assignment.updated_at.is_none());
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        // due_at for assignment_with_due_date() is 2026-09-01T23:59:00Z
        assert!(got.writes[0].content.contains("DTSTAMP:20260901T235900Z"));
    }

    #[test]
    fn dtstamp_falls_back_to_due_at_when_updated_at_does_not_parse() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.updated_at = Some("not-a-date".into());
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("DTSTAMP:20260901T235900Z"));
    }

    #[test]
    fn planning_the_same_assignment_twice_produces_byte_identical_content() {
        let root = Path::new("/caldir");
        let a1 = assignment_with_due_date();
        let a2 = assignment_with_due_date();
        let before = plan(root, &course(), &[a1], &[], fixed_now(), &State::default());
        let after = plan(root, &course(), &[a2], &[], fixed_now(), &State::default());
        assert_eq!(before.writes[0].content, after.writes[0].content);
    }

    fn course_with(id: u64, name: &str) -> Course {
        Course {
            id,
            name: name.into(),
            course_code: None,
        }
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
            CourseResult::Synced { writes: 3 },
            CourseResult::Synced { writes: 2 },
        ];
        let summary = RunSummary::from_results(&results);
        assert_eq!(summary.synced, 2);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.writes, 5);
    }

    #[test]
    fn summary_counts_failed_courses_separately_from_synced() {
        let results = vec![
            CourseResult::Synced { writes: 1 },
            CourseResult::Failed,
            CourseResult::Synced { writes: 4 },
        ];
        let summary = RunSummary::from_results(&results);
        assert_eq!(summary.synced, 2);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.writes, 5);
    }

    #[test]
    fn all_courses_synced_concludes_success() {
        let summary = RunSummary {
            synced: 3,
            failed: 0,
            writes: 7,
        };
        assert_eq!(conclude(summary).unwrap(), RunOutcome::Success);
    }

    #[test]
    fn some_courses_failed_concludes_partial_failure() {
        let summary = RunSummary {
            synced: 2,
            failed: 1,
            writes: 4,
        };
        assert_eq!(conclude(summary).unwrap(), RunOutcome::PartialFailure);
    }

    #[test]
    fn all_courses_failed_concludes_total_failure() {
        let summary = RunSummary {
            synced: 0,
            failed: 3,
            writes: 0,
        };
        let err = conclude(summary).unwrap_err();
        assert_eq!(err.failed, 3);
    }
}
