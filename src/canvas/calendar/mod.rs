//! Pure planning of deadline calendar files for the calendar-sync flow, plus
//! the I/O executor that runs it end to end.
//!
//! [`plan`](fn@crate::canvas::calendar::plan) decides which calendar files should exist for a course's
//! assignments. It performs no I/O and reads no clock — the current instant
//! is injected by the caller. See `docs/specs/calendar-sync-flow.md` (D3, D4,
//! D6, D9, D10) for the design this implements. This module emits up to two
//! components per assignment, in sibling directories: a deadline `VTODO`,
//! carrying a `PRIORITY` derived from Canvas grading state
//! (`priority_for`,
//! spec D6, ticket 07); and, when `unlock_at` is present and before `due_at`,
//! an availability-window `VEVENT` spanning that interval (spec D3, ticket
//! 08), marked `STATUS:COMPLETED` when the caller's own submission for that
//! assignment is submitted or graded (spec D7, ticket 09).
//!
//! The deadline `VTODO` is written for a human to read in an aggregated task
//! list (`docs/specs/calendar-sync-flow.md`, D3 for the component's shape and
//! D13 for how far each field actually travels): its `SUMMARY` and the first
//! line of its `DESCRIPTION` share one formatter,
//! `deadline_label_raw`, so the course name and assignment title can never
//! disagree between the two; `deadline_description_escaped` adds the
//! availability window and the assignment link as plain text, because `caldir` forwards
//! neither `URL` nor `DTSTART` to Google Tasks and `DESCRIPTION` is the only
//! free-text field that survives the trip. It also carries `DTSTART` =
//! `unlock_at`, but only when `availability_start` allows it — the same
//! strictly-before-`due_at` predicate that decides whether a window `VEVENT`
//! exists at all, so the two collections can never disagree about whether an
//! assignment has a window (spec ID4, RFC 5545 §3.8.2.3). Text values on this
//! path go
//! through `vtodo_text` (CR normalization then RFC 5545 §3.3.11 escaping)
//! and every line through `fold_line` (§3.1, 75 octets). Both apply to the
//! `VTODO` only: `render_vevent` and the whole `windows` collection are out
//! of scope and emit exactly the bytes they always have.
//!
//! [`run_calendar`](crate::canvas::calendar::run_calendar) is the executor half (spec D10): it fetches active
//! courses and their assignments, calls [`plan`](fn@crate::canvas::calendar::plan), and applies the result to
//! disk with [`fsutil::atomic_write`](crate::core::fsutil::atomic_write). Per spec "Fuera del alcance de los
//! tests", the executor and the Canvas calls are untested here — the repo has
//! no HTTP mock server and this ticket does not add one. [`select_active_courses`](crate::canvas::calendar::select_active_courses)
//! is pulled out of the executor specifically so the one piece of decision
//! logic in it (course/`ignored_courses` selection) stays testable without a
//! network.
//!
//! Per-course resilience (spec ticket 11) has its own pure seam: a course
//! whose assignment fetch fails is recorded as [`CourseResult::Failed`](crate::canvas::calendar::CourseResult::Failed)
//! instead of aborting the run, [`RunSummary::from_results`](crate::canvas::calendar::RunSummary::from_results) folds every
//! course's result into totals, and [`conclude`](crate::canvas::calendar::conclude) turns that fold into a
//! [`RunOutcome`](crate::canvas::calendar::RunOutcome) (or a [`TotalFailure`](crate::canvas::calendar::TotalFailure) when every course failed). All three
//! are plain data in, data out — testable without touching the network.

mod exec;
mod ics;
mod identity;
mod plan;
#[cfg(test)]
mod test_support;
mod vevent;
mod vtodo;

use std::path::PathBuf;

pub use exec::{
    conclude, run_calendar, select_active_courses, CourseResult, RunOutcome, RunSummary,
    TotalFailure,
};
pub use plan::{plan, plan_for_course};

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
    /// be) compared against — [`calendar_state_key`](identity::calendar_state_key) for a deadline `VTODO`,
    /// [`window_state_key`](identity::window_state_key) for an availability-window `VEVENT`. Two
    /// components can share an `assignment_id` but never this key: that is
    /// what stops recording one from marking the other "unchanged".
    pub state_key: String,
}

/// One calendar file to delete, in full (spec D5, ticket 10): the assignment
/// it belongs to no longer appears in Canvas's response, so its projected
/// component must stop existing on disk — `caldir push` propagates the
/// removal to the CalDAV server (spec D8; verified empirically by the
/// ticket 01 spike), so it is enough for u_crawler to remove the local file.
///
/// Carries the same `assignment_id`/`state_key` pairing as [`PlannedWrite`]
/// so the executor can, after removing the file, also drop the matching
/// `state.json` entry ([`record_deletes`](exec::record_deletes)) — without that, the same
/// assignment would look "deleted" again on every subsequent run forever,
/// since its old state entry would keep failing to match anything in a
/// Canvas response that (rightly) no longer mentions it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDelete {
    pub path: PathBuf,
    pub assignment_id: u64,
    pub state_key: String,
}

/// The outcome of planning: which files to write and which to delete.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Plan {
    pub writes: Vec<PlannedWrite>,
    pub deletes: Vec<PlannedDelete>,
}
