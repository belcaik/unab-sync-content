//! Pure planning of deadline calendar files for the calendar-sync flow, plus
//! the I/O executor that runs it end to end.
//!
//! [`plan`] decides which calendar files should exist for a course's
//! assignments. It performs no I/O and reads no clock — the current instant
//! is injected by the caller. See `docs/specs/calendar-sync-flow.md` (D3, D4,
//! D6, D9, D10) for the design this implements. This module emits up to two
//! components per assignment, in sibling directories: a deadline `VTODO`,
//! carrying a `PRIORITY` derived from Canvas grading state ([`priority_for`],
//! spec D6, ticket 07); and, when `unlock_at` is present and before `due_at`,
//! an availability-window `VEVENT` spanning that interval (spec D3, ticket
//! 08). Submission/completed status is later work that widens the same seam.
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
use sha1::{Digest, Sha1};

use crate::canvas::{Assignment, CanvasClient, Course};
use crate::config::Config;
use crate::fsutil;
use crate::state::{ItemState, State};
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
    /// The Canvas assignment this write projects. Carried alongside `path`/
    /// `content` so the executor can record the write against the same
    /// state key `plan` compared it against, without having to
    /// reverse-engineer an id out of a filename.
    pub assignment_id: u64,
    /// The `state.json` key this write's content hash was (and must again
    /// be) compared against — [`calendar_state_key`] for a deadline `VTODO`,
    /// [`window_state_key`] for an availability-window `VEVENT`. Two
    /// components can share an `assignment_id` but never this key: that is
    /// what stops recording one from marking the other "unchanged".
    pub state_key: String,
}

/// The outcome of planning: which files to write and which to delete.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub writes: Vec<PlannedWrite>,
    pub deletes: Vec<PathBuf>,
}

/// The on-disk name of the directory holding deadline `VTODO` files for a
/// course. A sibling directory holds the availability-window `VEVENT`s
/// ([`WINDOWS_DIR`]); a third semantics (recurring classes, spec D4) has room
/// to land later without moving anything under either.
const DEADLINES_DIR: &str = "deadlines";

/// The on-disk name of the directory holding availability-window `VEVENT`
/// files for a course (spec D4, ticket 08). Sibling of [`DEADLINES_DIR`], not
/// nested under it — the point is that a client can subscribe to one and not
/// the other.
const WINDOWS_DIR: &str = "windows";

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

/// Derive the calendar UID for an assignment's availability-window `VEVENT`
/// (spec D4, ticket 08). The `window-` discriminator is what keeps this
/// distinct from [`deadline_uid`]'s `todo-` for the same assignment id — see
/// that function's doc comment for why the distinction is load-bearing
/// (Radicale issue #101, cited in the spec). Fixed by ticket 04's plan; do not
/// change it.
fn window_uid(assignment_id: u64) -> String {
    format!("u_crawler-window-{assignment_id}@u-crawler.local")
}

/// Derive the on-disk filename for an assignment's availability-window
/// `VEVENT`. Lives under [`WINDOWS_DIR`], a different directory from
/// [`deadline_filename`]'s [`DEADLINES_DIR`], so the two never collide on
/// disk even though both derive their name the same way from the assignment
/// id.
fn window_filename(assignment_id: u64) -> String {
    format!("assignment-{assignment_id}.ics")
}

/// The `state.json` namespace for a deadline `VTODO`'s projected content
/// (spec D5): `calendar:{assignment_id}`. Distinct from the content flow's
/// own `assignment:{id}` key (`syncer.rs`) — same assignment, different
/// projection, so a separate key stops the two flows from clobbering each
/// other's bookkeeping.
fn calendar_state_key(assignment_id: u64) -> String {
    format!("calendar:{assignment_id}")
}

/// The `state.json` namespace for an availability-window `VEVENT`'s projected
/// content (spec D5, ticket 08): `calendar-window:{assignment_id}`. Distinct
/// from [`calendar_state_key`]'s `calendar:{assignment_id}` — the VTODO and
/// VEVENT for the same assignment are two different projections with two
/// different hashes, and sharing one key would make writing one component
/// mark the other "unchanged" (or vice versa) regardless of its own content.
fn window_state_key(assignment_id: u64) -> String {
    format!("calendar-window:{assignment_id}")
}

/// Hash rendered content the same way `syncer.rs` hashes markdown before
/// deciding whether to skip a write: SHA-1, hex-encoded. Not cryptographic —
/// just a cheap, stable fingerprint of "did the projected component change".
fn content_hash(content: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
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

/// Whether an assignment accepts a submission at all (spec D6/ticket 07).
///
/// Canvas signals "no submission" through `submission_types`: `"none"` (pure
/// reading/informational) and `"not_graded"` (explicitly marked not
/// gradable) are the values that mean nothing can be turned in. Anything else
/// present in the list — `online_upload`, `on_paper`, `discussion_topic`,
/// etc. — means a submission is accepted. A missing `submission_types`
/// (`None`, e.g. an older Canvas payload) is treated the same as "no
/// submission": we have no positive signal that one is accepted, so it does
/// not get the benefit of the doubt.
fn accepts_submission(submission_types: Option<&[String]>) -> bool {
    match submission_types {
        None => false,
        Some(types) => types.iter().any(|t| t != "none" && t != "not_graded"),
    }
}

/// Map an assignment to its `PRIORITY` value per the decided table (spec D6,
/// ticket 07). `1` is RFC 5545's highest priority; `0` ("undefined") is never
/// returned. Depends only on `points_possible`, `omit_from_final_grade` and
/// `submission_types` — all Canvas state — and deliberately takes no `now`,
/// so it cannot change on its own between runs (spec D6, D5, user story 14).
fn priority_for(assignment: &Assignment) -> u8 {
    if !accepts_submission(assignment.submission_types.as_deref()) {
        // Accepts no submission: reading, informational, or not gradable.
        return 9;
    }
    let counts_toward_final_grade = assignment.points_possible.unwrap_or(0.0) > 0.0
        && assignment.omit_from_final_grade != Some(true);
    if counts_toward_final_grade {
        1
    } else {
        // Accepts a submission but does not weigh on the final grade.
        5
    }
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
        format!("PRIORITY:{}", priority_for(assignment)),
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

/// Render a single assignment's availability window as a complete `.ics`
/// file: one `VCALENDAR` wrapping one `VEVENT` spanning `unlock` to `due`
/// (spec D3, ticket 08). No `PRIORITY` or `STATUS` — those are `VTODO`
/// concepts (submission/completed status is ticket 09's scope, not this
/// one's).
fn render_vevent(
    uid: &str,
    assignment: &Assignment,
    unlock: DateTime<Utc>,
    due: DateTime<Utc>,
) -> String {
    let dtstamp = dtstamp_for(assignment, due);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//u_crawler//calendar-sync//EN".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(dtstamp)),
        format!("DTSTART:{}", ics_datetime(unlock)),
        format!("DTEND:{}", ics_datetime(due)),
        format!(
            "SUMMARY:{}",
            escape_text(assignment.name.as_deref().unwrap_or(""))
        ),
    ];
    if let Some(url) = &assignment.html_url {
        lines.push(format!("URL:{}", escape_text(url)));
    }
    lines.push("END:VEVENT".to_string());
    lines.push("END:VCALENDAR".to_string());
    lines.join("\r\n") + "\r\n"
}

/// Plan the calendar files for one course: a deadline `VTODO` per assignment
/// with a due date, plus an availability-window `VEVENT` for each such
/// assignment that also has an `unlock_at` strictly before its `due_at`
/// (spec D3, ticket 08).
///
/// Pure: no network, no disk access and no system clock — `now` is the
/// caller's injected instant. `submissions` is accepted so later tickets can
/// widen this function without changing every call site; this ticket does
/// not build logic on it yet.
///
/// `prev` is the previous run's persisted state (spec D5): for each
/// component, the rendered content is hashed and compared against `prev`'s
/// entry under that component's own state key — [`calendar_state_key`] for
/// the `VTODO`, [`window_state_key`] for the `VEVENT`. The two live in
/// distinct namespaces, so writing one never marks the other "unchanged". A
/// match means Canvas's projected component is unchanged, so **no write is
/// planned** for it — this is what keeps an unchanged run's plan empty (spec
/// user story 14), and it holds independently per component: an assignment
/// can plan a window write while its deadline is unchanged, or vice versa. A
/// mismatch (including "no entry yet") plans a write with the freshly
/// rendered content, Canvas winning per D5.
///
/// The filename and UID both derive only from the assignment id (see
/// [`deadline_uid`]/[`deadline_filename`] and [`window_uid`]/
/// [`window_filename`]), never from any date, so a due-date or unlock-date
/// change rewrites the same path rather than orphaning a differently-named
/// old file — the sharp case ticket 06 names is designed out at the source,
/// not cleaned up here. A title change is exactly the same code path: the
/// hash changes, the path does not.
///
/// No `unlock_at` at all produces the `VTODO` and no `VEVENT` — there is no
/// window to represent. An `unlock_at` at or after `due_at` is treated as
/// inconsistent Canvas data and also produces no `VEVENT`, rather than a
/// zero- or negative-length event. An assignment with no `due_at` produces
/// neither component: a window needs both ends, so it cannot exist without a
/// due date either.
pub fn plan(
    caldir_root: &Path,
    course: &Course,
    assignments: &[Assignment],
    _submissions: &[Submission],
    _now: DateTime<Utc>,
    prev: &State,
) -> Plan {
    let course_dir = fsutil::course_dir(caldir_root, course);
    let deadlines_dir = course_dir.join(DEADLINES_DIR);
    let windows_dir = course_dir.join(WINDOWS_DIR);
    let mut writes = Vec::new();
    for assignment in assignments {
        let Some(due) = assignment.due_at else {
            continue;
        };

        let uid = deadline_uid(assignment.id);
        let path = deadlines_dir.join(deadline_filename(assignment.id));
        let content = render_vtodo(&uid, assignment, due);
        let hash = content_hash(&content);
        let key = calendar_state_key(assignment.id);
        let unchanged =
            prev.get(&key).and_then(|item| item.content_hash.as_deref()) == Some(hash.as_str());
        if !unchanged {
            writes.push(PlannedWrite {
                path,
                content,
                assignment_id: assignment.id,
                state_key: key,
            });
        }

        // Availability window (spec D3, ticket 08): only when `unlock_at` is
        // present and strictly before `due_at`. Absent `unlock_at` means
        // there is no window to represent; an `unlock_at` at or after `due_at`
        // is inconsistent Canvas data and is treated as "no window" rather
        // than emitting a zero/negative-length event.
        if let Some(unlock) = assignment.unlock_at {
            if unlock < due {
                let window_uid = window_uid(assignment.id);
                let window_path = windows_dir.join(window_filename(assignment.id));
                let window_content = render_vevent(&window_uid, assignment, unlock, due);
                let window_hash = content_hash(&window_content);
                let window_key = window_state_key(assignment.id);
                let window_unchanged = prev
                    .get(&window_key)
                    .and_then(|item| item.content_hash.as_deref())
                    == Some(window_hash.as_str());
                if !window_unchanged {
                    writes.push(PlannedWrite {
                        path: window_path,
                        content: window_content,
                        assignment_id: assignment.id,
                        state_key: window_key,
                    });
                }
            }
        }
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
/// Submission data is not wired in yet — submission status is ticket 09's
/// scope, so this always plans with an empty submission list. The previous
/// [`State`] *is* wired in (ticket 06): it is loaded from the course's
/// existing `state.json` under `download_root` — the same file `sync` and
/// `announcements` already read and write, under the `calendar:{id}`
/// namespace (spec D5) — never from anywhere inside `caldir_root`. After a
/// successful apply, every written assignment's content hash is recorded
/// back into that state and saved, so the next run's [`plan`] call has
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

        let state_path = fsutil::course_dir(&download_root, course).join("state.json");
        let mut state = State::load(&state_path).await;

        let course_plan = plan(&caldir_root, course, &assignments, &[], now, &state);

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

        match apply_plan(course.id, &course_plan).await {
            Ok(()) => {
                record_writes(&mut state, &course_plan.writes);
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
        "{}Calendar: {} deadline file(s) across {} course(s) synced, {} failed",
        if dry_run { "DRY-RUN: " } else { "" },
        summary.writes,
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
        match tokio::fs::remove_file(delete).await {
            Ok(()) => tracing::info!(course_id, path = %delete.display(), "removed calendar file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Record every successfully-applied write's content hash back into `state`,
/// under its own `state_key` (deadline or window — see [`PlannedWrite`]), so
/// the next run's [`plan`] call sees what was actually written and can
/// produce an empty plan when Canvas hasn't changed. Does not save to disk —
/// the caller does that, and only outside `--dry-run`.
fn record_writes(state: &mut State, writes: &[PlannedWrite]) {
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

    // --- priority heuristic (ticket 07, spec D6) ---
    //
    // One test per row of the decided table, plus the case the ticket calls
    // out by name: points_possible > 0 but omitted from the final grade must
    // NOT get the highest priority. Asserted through the Plan's rendered
    // content (seam S3), never by calling `priority_for` directly.

    #[test]
    fn graded_and_not_omitted_gets_priority_1() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(30.0);
        assignment.omit_from_final_grade = Some(false);
        assignment.submission_types = Some(vec!["online_upload".into()]);
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("PRIORITY:1"));
    }

    #[test]
    fn accepts_submission_but_zero_points_gets_priority_5() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(0.0);
        assignment.omit_from_final_grade = None;
        assignment.submission_types = Some(vec!["online_upload".into()]);
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("PRIORITY:5"));
    }

    #[test]
    fn no_submission_type_gets_priority_9() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(30.0);
        assignment.omit_from_final_grade = Some(false);
        assignment.submission_types = Some(vec!["none".into()]);
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("PRIORITY:9"));
    }

    #[test]
    fn not_gradable_submission_type_also_gets_priority_9() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(30.0);
        assignment.omit_from_final_grade = Some(false);
        assignment.submission_types = Some(vec!["not_graded".into()]);
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("PRIORITY:9"));
    }

    #[test]
    fn missing_submission_types_gets_priority_9() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(30.0);
        assignment.omit_from_final_grade = Some(false);
        assignment.submission_types = None;
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(got.writes[0].content.contains("PRIORITY:9"));
    }

    #[test]
    fn points_possible_but_omitted_from_final_grade_is_not_priority_1() {
        // The case a naive implementation gets wrong (ticket 07's explicit
        // callout): a positive point value does not win if the assignment is
        // marked as excluded from the final grade.
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.points_possible = Some(30.0);
        assignment.omit_from_final_grade = Some(true);
        assignment.submission_types = Some(vec!["online_upload".into()]);
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert!(!got.writes[0].content.contains("PRIORITY:1"));
        assert!(got.writes[0].content.contains("PRIORITY:5"));
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

    // --- availability window (ticket 08, spec D3) ---
    //
    // The three date cases the ticket names, plus confirmation that the
    // "unchanged -> empty plan" guarantee (ticket 06) still holds once two
    // components exist per assignment.

    #[test]
    fn unlock_before_due_produces_a_vevent_in_a_sibling_windows_directory() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .into(),
        );
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        // The VTODO is still emitted, plus the VEVENT.
        assert_eq!(got.writes.len(), 2);

        let vtodo = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VTODO"))
            .expect("vtodo present");
        assert_eq!(
            vtodo.path,
            Path::new("/caldir/Intro_to_Testing_TST101/deadlines/assignment-555.ics")
        );

        let vevent = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VEVENT"))
            .expect("vevent present");
        assert_eq!(
            vevent.path,
            Path::new("/caldir/Intro_to_Testing_TST101/windows/assignment-555.ics")
        );
        assert!(vevent
            .content
            .contains("UID:u_crawler-window-555@u-crawler.local"));
        assert!(vevent.content.contains("DTSTART:20260801T000000Z"));
        assert!(vevent.content.contains("DTEND:20260901T235900Z"));
        assert!(vevent.content.contains("SUMMARY:Essay Draft"));
        assert!(vevent.content.contains("END:VEVENT"));
        // Distinguishable from the VTODO UID for the same assignment.
        assert_ne!(vevent.content, vtodo.content);
        assert!(!vevent
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
    }

    #[test]
    fn missing_unlock_at_produces_the_vtodo_and_no_vevent() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        assert!(assignment.unlock_at.is_none());
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        assert_eq!(got.writes.len(), 1);
        assert!(got.writes[0].content.contains("BEGIN:VTODO"));
        assert!(!got
            .writes
            .iter()
            .any(|w| w.content.contains("BEGIN:VEVENT")));
    }

    #[test]
    fn unlock_at_after_due_at_is_inconsistent_and_produces_no_vevent() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        // due_at for assignment_with_due_date() is 2026-09-01T23:59:00Z
        assignment.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-09-15T00:00:00Z")
                .unwrap()
                .into(),
        );
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        assert_eq!(got.writes.len(), 1);
        assert!(got.writes[0].content.contains("BEGIN:VTODO"));
        assert!(!got
            .writes
            .iter()
            .any(|w| w.content.contains("BEGIN:VEVENT")));
    }

    #[test]
    fn unchanged_vtodo_and_vevent_together_produce_an_empty_plan() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .into(),
        );

        let first = plan(
            root,
            &course(),
            std::slice::from_ref(&assignment),
            &[],
            fixed_now(),
            &State::default(),
        );
        assert_eq!(first.writes.len(), 2);

        let mut prev = State::default();
        for write in &first.writes {
            prev.set(
                write.state_key.clone(),
                ItemState {
                    etag: None,
                    updated_at: None,
                    size: None,
                    content_hash: Some(content_hash(&write.content)),
                    last_error: None,
                    error_count: None,
                },
            );
        }

        let second = plan(root, &course(), &[assignment], &[], fixed_now(), &prev);

        assert!(second.writes.is_empty());
        assert!(second.deletes.is_empty());
    }

    // --- idempotency and date-change handling (ticket 06) ---

    /// Build a `State` recording, under the `calendar:{id}` namespace, a
    /// content hash computed independently of `plan`'s own `content_hash`
    /// helper (using the `sha1` crate directly, the same way `syncer.rs`
    /// does), so this does not just recompute the production code's answer
    /// and compare it to itself.
    fn state_with_hash(assignment_id: u64, content: &str) -> State {
        let mut hasher = Sha1::new();
        hasher.update(content.as_bytes());
        let hash = hex::encode(hasher.finalize());
        let mut state = State::default();
        state.set(
            format!("calendar:{assignment_id}"),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some(hash),
                last_error: None,
                error_count: None,
            },
        );
        state
    }

    #[test]
    fn no_changes_since_previous_state_produces_an_empty_plan() {
        // This is the test the ticket calls out by name: "es la garantía de
        // todo el comportamiento de este ticket." A first run captures what
        // would be written; a previous state recording that exact content
        // must make the second run plan nothing at all.
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
        assert_eq!(first.writes.len(), 1);
        let prev = state_with_hash(assignment.id, &first.writes[0].content);

        let second = plan(root, &course(), &[assignment], &[], fixed_now(), &prev);

        assert!(second.writes.is_empty());
        assert!(second.deletes.is_empty());
    }

    #[test]
    fn due_date_change_produces_a_write() {
        let root = Path::new("/caldir");
        let original = assignment_with_due_date();
        let first = plan(
            root,
            &course(),
            std::slice::from_ref(&original),
            &[],
            fixed_now(),
            &State::default(),
        );
        let prev = state_with_hash(original.id, &first.writes[0].content);

        let mut rescheduled = original;
        rescheduled.due_at = Some(
            DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
                .unwrap()
                .into(),
        );

        let got = plan(root, &course(), &[rescheduled], &[], fixed_now(), &prev);

        assert_eq!(got.writes.len(), 1);
        assert!(got.writes[0].content.contains("DUE:20261015T120000Z"));
        // The vacuous box: the path never depends on the due date (it is
        // derived only from the assignment id), so a date change can never
        // orphan a previously-written file under a different name — there
        // is nothing to delete.
        assert!(got.deletes.is_empty());
    }

    #[test]
    fn title_change_produces_a_write_with_same_uid_and_path() {
        let root = Path::new("/caldir");
        let original = assignment_with_due_date();
        let first = plan(
            root,
            &course(),
            std::slice::from_ref(&original),
            &[],
            fixed_now(),
            &State::default(),
        );
        let prev = state_with_hash(original.id, &first.writes[0].content);

        let mut renamed = original;
        renamed.name = Some("Essay Final".into());

        let got = plan(root, &course(), &[renamed], &[], fixed_now(), &prev);

        assert_eq!(got.writes.len(), 1);
        assert_eq!(got.writes[0].path, first.writes[0].path);
        assert!(got.writes[0]
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert!(got.writes[0].content.contains("SUMMARY:Essay Final"));
        assert!(got.deletes.is_empty());
    }

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
        assert_eq!(stored.content_hash, Some(content_hash(&writes[0].content)));
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
            deletes: vec![path],
        };

        let result = apply_plan(1, &course_plan).await;

        assert!(result.is_ok());
    }
}
