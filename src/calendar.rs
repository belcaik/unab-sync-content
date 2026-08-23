//! Pure planning of deadline calendar files for the calendar-sync flow.
//!
//! [`plan`] decides which calendar files should exist for a course's
//! assignment deadlines. It performs no I/O and reads no clock — the current
//! instant is injected by the caller. See `docs/specs/calendar-sync-flow.md`
//! (D3, D4, D9, D10) for the design this implements. This module only
//! understands deadlines (`VTODO`); priority, availability windows and
//! submission status are later work that widens the same seam.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::canvas::{Assignment, Course};
use crate::fsutil;
use crate::state::State;

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
}
