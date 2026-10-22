//! The pure planner: which calendar files should exist for a course's
//! assignments, which to (re)write and which to delete. No I/O, no clock.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::ics::availability_start;
use super::identity::{
    calendar_state_key, component_filename, deadline_filename, deadline_uid, window_filename,
    window_state_key, window_uid, CALENDAR_KEY_PREFIX, DEADLINES_DIR, WINDOWS_DIR,
    WINDOW_KEY_PREFIX,
};
use super::vevent::render_vevent;
use super::vtodo::{is_submission_done, render_vtodo};
use super::{Plan, PlannedDelete, PlannedWrite};
use crate::canvas::course::course_dir;
use crate::canvas::{Assignment, Course, Submission};
use crate::core::state::{content_hash, State};

/// Index submissions by assignment id for O(1) lookup while planning. Not a
/// `HashMap<u64, Submission>` in the public API — this is planning-internal
/// bookkeeping, not part of [`plan`]'s contract.
fn index_submissions(submissions: &[Submission]) -> HashMap<u64, &Submission> {
    submissions.iter().map(|s| (s.assignment_id, s)).collect()
}

/// Plan the calendar files for one course: a deadline `VTODO` per assignment
/// with a due date, plus an availability-window `VEVENT` for each such
/// assignment that also has an `unlock_at` strictly before its `due_at`
/// (spec D3, ticket 08).
///
/// Pure: no network, no disk access and no system clock. `submissions` is the
/// caller's own submissions
/// for this course (spec D1, ticket 09): a deadline `VTODO` is marked
/// `STATUS:COMPLETED` when [`is_submission_done`](super::vtodo::is_submission_done) says so for the matching
/// assignment id, per D7 ("submitted, or graded"). No group-assignment
/// special-casing lives here — see [`is_submission_done`](super::vtodo::is_submission_done)'s doc comment for
/// why the bulk `self` query already covers it.
///
/// `prev` is the previous run's persisted state (spec D5): for each
/// component, the rendered content is hashed and compared against `prev`'s
/// entry under that component's own state key — [`calendar_state_key`](super::identity::calendar_state_key) for
/// the `VTODO`, [`window_state_key`](super::identity::window_state_key) for the `VEVENT`. The two live in
/// distinct namespaces, so writing one never marks the other "unchanged". A
/// match means Canvas's projected component is unchanged, so **no write is
/// planned** for it — this is what keeps an unchanged run's plan empty (spec
/// user story 14), and it holds independently per component: an assignment
/// can plan a window write while its deadline is unchanged, or vice versa. A
/// mismatch (including "no entry yet") plans a write with the freshly
/// rendered content, Canvas winning per D5.
///
/// The filename and UID both derive only from the assignment id (see
/// [`deadline_uid`](super::identity::deadline_uid)/[`deadline_filename`](super::identity::deadline_filename) and [`window_uid`](super::identity::window_uid)/
/// [`window_filename`](super::identity::window_filename)), never from any date, so a due-date or unlock-date
/// change rewrites the same path rather than orphaning a differently-named
/// old file — the sharp case ticket 06 names is designed out at the source,
/// not cleaned up here. A title change is exactly the same code path: the
/// hash changes, the path does not.
///
/// No `unlock_at` at all produces the `VTODO` and no `VEVENT` — there is no
/// window to represent. An `unlock_at` at or after `due_at` is treated as
/// inconsistent Canvas data and also produces no `VEVENT`, rather than a
/// zero- or negative-length event. In both of those cases the `VTODO` it does
/// produce carries no `DTSTART` either, since [`availability_start`](super::ics::availability_start) is the
/// single predicate behind both decisions — though its `DESCRIPTION` still
/// reports the real `unlock_at` Canvas gave (spec ID5): the property is
/// omitted because the RFC forces it, not because the datum is disbelieved.
/// An assignment with no `due_at` produces
/// neither component: a window needs both ends, so it cannot exist without a
/// due date either.
///
/// **Deletion (spec D5, ticket 10).** Canvas is the source of truth: what
/// `assignments` does not mention today is not in the calendar. After
/// planning writes, this scans `prev` for every `calendar:{id}` and
/// `calendar-window:{id}` entry and, for each whose `{id}` is not the id of
/// any assignment in `assignments`, plans a delete of that component's file
/// — both the `VTODO` and the `VEVENT` when both existed, since they are
/// tracked under separate state keys and neither implies the other. This is
/// why `plan` takes the *whole* `assignments` list for the course, not just
/// the ones with a `due_at`: an assignment id absent from it entirely is
/// what "deleted from Canvas" means here, deliberately narrower than "lost
/// its due date" (out of this ticket's contract).
///
/// **Safety.** `plan` trusts `assignments` completely — it has no way to
/// tell "Canvas returned zero assignments" apart from "the fetch failed and
/// an empty list was substituted for it". That distinction has to be made
/// by the caller *before* calling `plan`, which is exactly what
/// [`plan_for_course`] exists to make into a testable, non-optional step:
/// see its doc comment for how a failed fetch is kept from ever reaching
/// this function.
pub fn plan(
    caldir_root: &Path,
    course: &Course,
    assignments: &[Assignment],
    submissions: &[Submission],
    prev: &State,
) -> Plan {
    let course_dir = course_dir(caldir_root, course);
    Plan {
        writes: plan_writes(&course_dir, course, assignments, submissions, prev),
        deletes: plan_deletes(&course_dir, assignments, prev),
    }
}

/// The write half of [`plan`]: a deadline `VTODO` per assignment with a due
/// date, plus an availability-window `VEVENT` for each such assignment that
/// also has an `unlock_at` strictly before its `due_at` (spec D3, ticket 08).
fn plan_writes(
    course_dir: &Path,
    course: &Course,
    assignments: &[Assignment],
    submissions: &[Submission],
    prev: &State,
) -> Vec<PlannedWrite> {
    let deadlines_dir = course_dir.join(DEADLINES_DIR);
    let windows_dir = course_dir.join(WINDOWS_DIR);
    let submissions_by_assignment = index_submissions(submissions);
    let mut writes = Vec::new();
    for assignment in assignments {
        let Some(due) = assignment.due_at else {
            continue;
        };

        let done = submissions_by_assignment
            .get(&assignment.id)
            .is_some_and(|s| is_submission_done(s));

        plan_component(
            &mut writes,
            deadlines_dir.join(deadline_filename(assignment.id)),
            render_vtodo(&deadline_uid(assignment.id), course, assignment, due, done),
            assignment.id,
            calendar_state_key(assignment.id),
            prev,
        );

        // Availability window (spec D3, ticket 08): only when `unlock_at` is
        // present and strictly before `due_at`. Absent `unlock_at` means
        // there is no window to represent; an `unlock_at` at or after `due_at`
        // is inconsistent Canvas data and is treated as "no window" rather
        // than emitting a zero/negative-length event. That is the very same
        // [`availability_start`] the `VTODO` consults for its `DTSTART`, so
        // the two collections cannot drift into disagreeing about whether
        // this assignment has a window.
        if let Some(unlock) = availability_start(assignment.unlock_at, due) {
            plan_component(
                &mut writes,
                windows_dir.join(window_filename(assignment.id)),
                render_vevent(&window_uid(assignment.id), assignment, unlock, due),
                assignment.id,
                window_state_key(assignment.id),
                prev,
            );
        }
    }
    writes
}

/// Hash one rendered component, compare it against `prev` under `state_key`,
/// and push its write when the content differs (or was never recorded). A
/// match means Canvas's projected component is unchanged, so **no write is
/// planned** for it (spec D5, user story 14).
fn plan_component(
    writes: &mut Vec<PlannedWrite>,
    path: PathBuf,
    content: String,
    assignment_id: u64,
    state_key: String,
    prev: &State,
) {
    let hash = content_hash(content.as_bytes());
    let unchanged = prev
        .get(&state_key)
        .and_then(|item| item.content_hash.as_deref())
        == Some(hash.as_str());
    if !unchanged {
        writes.push(PlannedWrite {
            path,
            content,
            assignment_id,
            state_key,
        });
    }
}

/// The delete half of [`plan`]: reconciliation (spec D5, ticket 10). Anything
/// `prev` remembers under a calendar namespace whose assignment id is no
/// longer in `assignments` has been deleted from Canvas, so its file must go
/// too.
fn plan_deletes(course_dir: &Path, assignments: &[Assignment], prev: &State) -> Vec<PlannedDelete> {
    let current_ids: HashSet<u64> = assignments.iter().map(|a| a.id).collect();
    let deadlines_dir = course_dir.join(DEADLINES_DIR);
    let windows_dir = course_dir.join(WINDOWS_DIR);
    let mut deletes = Vec::new();
    for key in prev.items.keys() {
        if key.starts_with(CALENDAR_KEY_PREFIX) {
            plan_delete(
                &mut deletes,
                key,
                CALENDAR_KEY_PREFIX,
                &deadlines_dir,
                &current_ids,
            );
        } else if key.starts_with(WINDOW_KEY_PREFIX) {
            plan_delete(
                &mut deletes,
                key,
                WINDOW_KEY_PREFIX,
                &windows_dir,
                &current_ids,
            );
        }
    }
    deletes
}

/// Plan the deletion of one calendar state entry whose assignment id is gone
/// from Canvas: strip `prefix`, parse the id, and push the delete for the
/// file under `dir`.
fn plan_delete(
    deletes: &mut Vec<PlannedDelete>,
    key: &str,
    prefix: &str,
    dir: &Path,
    current_ids: &HashSet<u64>,
) {
    if let Some(id) = key.strip_prefix(prefix).and_then(|s| s.parse::<u64>().ok()) {
        if !current_ids.contains(&id) {
            deletes.push(PlannedDelete {
                path: dir.join(component_filename(id)),
                assignment_id: id,
                state_key: key.to_string(),
            });
        }
    }
}

/// The "fetch failed" marker for [`plan_for_course`]'s parameters. Carries
/// nothing: the caller has already logged the real error by the time it gets
/// here — only the fact of failure matters for the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FetchFailed;

/// Gate [`plan`] on both of a course's fetches having actually succeeded
/// (spec ticket 10's safety requirement): "un ramo que falla al consultarse
/// NO dispara el borrado de sus componentes". `plan` itself cannot make this
/// distinction — an empty `Vec<Assignment>` and a failed fetch look
/// identical once they are both just `&[Assignment]` — so the two have to be
/// told apart *before* `plan` is called, not inside it. `Err(FetchFailed)`
/// stands in for "the fetch failed"; the caller has already logged the real
/// error by the time it gets here; only the fact of failure matters for this
/// decision.
///
/// Returns `None` — no plan attempted at all, not an empty one — when either
/// fetch failed, so a network blip can never be mistaken for "Canvas says
/// this course now has zero assignments" and wipe every calendar file for
/// it. `run_calendar` is the only caller; extracted here (alongside
/// [`conclude`](super::exec::conclude)) so the guarantee is a
/// testable predicate rather than something only visible by reading
/// `run_calendar`'s control flow.
pub(super) fn plan_for_course(
    caldir_root: &Path,
    course: &Course,
    assignments: Result<Vec<Assignment>, FetchFailed>,
    submissions: Result<Vec<Submission>, FetchFailed>,
    prev: &State,
) -> Option<Plan> {
    let assignments = assignments.ok()?;
    let submissions = submissions.ok()?;
    Some(plan(caldir_root, course, &assignments, &submissions, prev))
}

#[cfg(test)]
mod tests {
    use super::{plan, plan_for_course, FetchFailed};
    use crate::canvas::calendar::exec::record_writes;
    use crate::canvas::calendar::identity::{calendar_state_key, deadline_uid, window_state_key};
    use crate::canvas::calendar::test_support::{
        assignment_with_due_date, assignment_without_due_date, course, fixture_assignment,
        fixture_course, logical_line, only_vtodo, read_fixture, state_with_hash,
        submission_submitted,
    };
    use crate::canvas::calendar::vtodo::render_vtodo;
    use crate::canvas::calendar::{Plan, PlannedDelete};
    use crate::canvas::{Assignment, Course, Submission};
    use crate::core::state::{content_hash, ItemState, State};
    use chrono::DateTime;
    use std::path::{Path, PathBuf};

    #[test]
    fn empty_assignment_set_produces_an_empty_plan() {
        let root = Path::new("/caldir");
        let got = plan(root, &course(), &[], &[], &State::default());
        assert!(got.writes.is_empty());
        assert!(got.deletes.is_empty());
    }

    #[test]
    fn assignment_without_due_date_produces_no_components() {
        let root = Path::new("/caldir");
        let assignments = vec![assignment_without_due_date()];
        let got = plan(root, &course(), &assignments, &[], &State::default());
        assert!(got.writes.is_empty());
        assert!(got.deletes.is_empty());
    }

    #[test]
    fn assignment_with_due_date_produces_a_vtodo_write() {
        let root = Path::new("/caldir");
        let assignments = vec![assignment_with_due_date()];
        let got = plan(root, &course(), &assignments, &[], &State::default());
        assert_eq!(got.deletes, Vec::<PlannedDelete>::new());
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
        // Hand-written literal: `<course human name> - <assignment title>`
        // (spec ID1). The course name is `Course.name`, not the sanitized
        // directory `Intro_to_Testing_TST101` and not the course code.
        assert!(write
            .content
            .contains("SUMMARY:Intro to Testing - Essay Draft"));
        assert!(write
            .content
            .contains("URL:https://canvas.example.edu/courses/1/assignments/555"));
        assert!(write.content.contains("END:VTODO"));
    }

    #[test]
    fn a_fully_populated_assignment_renders_these_exact_vtodo_bytes() {
        // The whole component, hand-written. Nothing here is recomputed with
        // the renderer's own helpers, so a change to any of them — label,
        // escaping, date format, fold points, property order — has to be
        // restated here deliberately.
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .into(),
        );
        let got = plan(root, &course(), &[assignment], &[], &State::default());
        let vtodo = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VTODO"))
            .expect("vtodo present");

        assert_eq!(
            vtodo.content,
            concat!(
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "PRODID:-//u_crawler//calendar-sync//EN\r\n",
                "BEGIN:VTODO\r\n",
                "UID:u_crawler-todo-555@u-crawler.local\r\n",
                "DTSTAMP:20260901T235900Z\r\n",
                // `unlock_at` is strictly before `due_at` here, so the pair
                // is legal (§3.8.2.3) and `DTSTART` appears — immediately
                // ahead of `DUE`, in the same `DATE-TIME` UTC `Z` form.
                "DTSTART:20260801T000000Z\r\n",
                "DUE:20260901T235900Z\r\n",
                "PRIORITY:9\r\n",
                "SUMMARY:Intro to Testing - Essay Draft\r\n",
                r"DESCRIPTION:Intro to Testing - Essay Draft\nDisponible: 2026-08-01T00:00:00",
                "\r\n",
                r" Z - Vence: 2026-09-01T23:59:00Z\nhttps://canvas.example.edu/courses/1/assi",
                "\r\n",
                " gnments/555\r\n",
                "URL:https://canvas.example.edu/courses/1/assignments/555\r\n",
                "END:VTODO\r\n",
                "END:VCALENDAR\r\n",
            )
        );
    }

    #[test]
    fn the_window_vevent_bytes_are_unchanged_by_this_ticket() {
        // Scope contract (spec ID7/ID9): `windows` is out of scope, so its
        // component must be byte-identical to what it was before the `VTODO`
        // grew a label, a DESCRIPTION and line folding. Pinned in full, not
        // probed with `contains`, so any drift shows up here.
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
                .unwrap()
                .into(),
        );
        let got = plan(root, &course(), &[assignment], &[], &State::default());
        let vevent = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VEVENT"))
            .expect("vevent present");

        assert_eq!(
            vevent.content,
            concat!(
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "PRODID:-//u_crawler//calendar-sync//EN\r\n",
                "BEGIN:VEVENT\r\n",
                "UID:u_crawler-window-555@u-crawler.local\r\n",
                "DTSTAMP:20260901T235900Z\r\n",
                "DTSTART:20260801T000000Z\r\n",
                "DTEND:20260901T235900Z\r\n",
                // Bare assignment title: no course prefix, because the label
                // is a VTODO concern only.
                "SUMMARY:Essay Draft\r\n",
                "URL:https://canvas.example.edu/courses/1/assignments/555\r\n",
                "END:VEVENT\r\n",
                "END:VCALENDAR\r\n",
            )
        );
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

        let before = plan(root, &course(), &[original], &[], &State::default());
        let after = plan(
            root,
            &course(),
            &[renamed_and_rescheduled],
            &[],
            &State::default(),
        );

        assert_eq!(before.writes[0].path, after.writes[0].path);
        assert!(after.writes[0]
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert!(after.writes[0].content.contains("DUE:20261015T120000Z"));
    }

    #[test]
    fn planning_the_same_assignment_twice_produces_byte_identical_content() {
        let root = Path::new("/caldir");
        let a1 = assignment_with_due_date();
        let a2 = assignment_with_due_date();
        let before = plan(root, &course(), &[a1], &[], &State::default());
        let after = plan(root, &course(), &[a2], &[], &State::default());
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
        let got = plan(root, &course(), &[assignment], &[], &State::default());

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
        let got = plan(root, &course(), &[assignment], &[], &State::default());

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
        let got = plan(root, &course(), &[assignment], &[], &State::default());

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
                    content_hash: Some(content_hash(write.content.as_bytes())),
                    last_error: None,
                    error_count: None,
                },
            );
        }

        let second = plan(root, &course(), &[assignment], &[], &prev);

        assert!(second.writes.is_empty());
        assert!(second.deletes.is_empty());
    }

    // --- idempotency and date-change handling (ticket 06) ---

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
            &State::default(),
        );
        assert_eq!(first.writes.len(), 1);
        let prev = state_with_hash(assignment.id, &first.writes[0].content);

        let second = plan(root, &course(), &[assignment], &[], &prev);

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
            &State::default(),
        );
        let prev = state_with_hash(original.id, &first.writes[0].content);

        let mut rescheduled = original;
        rescheduled.due_at = Some(
            DateTime::parse_from_rfc3339("2026-10-15T12:00:00Z")
                .unwrap()
                .into(),
        );

        let got = plan(root, &course(), &[rescheduled], &[], &prev);

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
            &State::default(),
        );
        let prev = state_with_hash(original.id, &first.writes[0].content);

        let mut renamed = original;
        renamed.name = Some("Essay Final".into());

        let got = plan(root, &course(), &[renamed], &[], &prev);

        assert_eq!(got.writes.len(), 1);
        assert_eq!(got.writes[0].path, first.writes[0].path);
        assert!(got.writes[0]
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert!(got.writes[0]
            .content
            .contains("SUMMARY:Intro to Testing - Essay Final"));
        assert!(got.deletes.is_empty());
    }

    // --- identity, convergence and cross-field regression (ticket 15,
    // spec ID8) ---
    //
    // Everything below goes through `plan` and asserts on the returned
    // `Plan` and the `.ics` bytes it carries. Every expected value is a
    // hand-written literal; none is recomputed with a helper under test.

    #[test]
    fn the_compatibility_fixtures_render_these_exact_bytes() {
        // The independent expectation for the bytes committed under
        // `.scratch/calendar-rich-vtodo/fixtures/` and handed to caldir and
        // vassago as evidence (see that directory's README). Hand-written in
        // full, fold points included: the 75-octet cuts were derived from RFC
        // 5545 §3.1 by hand, not from `fold_line`'s output — which is exactly
        // why this literal must stay even though
        // `the_committed_fixture_files_still_match_what_the_planner_emits`
        // now reads those files off disk. That test binds the files to the
        // renderer; this one says what the bytes were always meant to be.
        let root = Path::new("/caldir");

        let with_window = plan(
            root,
            &fixture_course(),
            &[fixture_assignment()],
            &[],
            &State::default(),
        );
        let vtodo = only_vtodo(&with_window);
        assert_eq!(
            vtodo.path,
            Path::new("/caldir/Calculo_Diferencial_MAT1101/deadlines/assignment-90210.ics")
        );
        assert_eq!(
            vtodo.content,
            concat!(
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "PRODID:-//u_crawler//calendar-sync//EN\r\n",
                "BEGIN:VTODO\r\n",
                "UID:u_crawler-todo-90210@u-crawler.local\r\n",
                "DTSTAMP:20260901T183000Z\r\n",
                "DTSTART:20260909T140000Z\r\n",
                "DUE:20260916T235900Z\r\n",
                "PRIORITY:1\r\n",
                "SUMMARY:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\r\n",
                r"DESCRIPTION:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\nDisp",
                "\r\n",
                r" onible: 2026-09-09T14:00:00Z - Vence: 2026-09-16T23:59:00Z\nhttps://canvas",
                "\r\n",
                " .example.edu/courses/4210/assignments/90210\r\n",
                "URL:https://canvas.example.edu/courses/4210/assignments/90210\r\n",
                "END:VTODO\r\n",
                "END:VCALENDAR\r\n",
            )
        );

        let mut no_unlock = fixture_assignment();
        no_unlock.unlock_at = None;
        let without_window = plan(
            root,
            &fixture_course(),
            &[no_unlock],
            &[],
            &State::default(),
        );
        assert_eq!(without_window.writes.len(), 1, "no window component");
        assert_eq!(
            without_window.writes[0].content,
            concat!(
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "PRODID:-//u_crawler//calendar-sync//EN\r\n",
                "BEGIN:VTODO\r\n",
                "UID:u_crawler-todo-90210@u-crawler.local\r\n",
                "DTSTAMP:20260901T183000Z\r\n",
                // No `DTSTART`: Canvas gave no `unlock_at`, so there is no
                // window to assert (spec ID4).
                "DUE:20260916T235900Z\r\n",
                "PRIORITY:1\r\n",
                "SUMMARY:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\r\n",
                r"DESCRIPTION:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\nDisp",
                "\r\n",
                r" onible: sin fecha de apertura - Vence: 2026-09-16T23:59:00Z\nhttps://canva",
                "\r\n",
                " s.example.edu/courses/4210/assignments/90210\r\n",
                "URL:https://canvas.example.edu/courses/4210/assignments/90210\r\n",
                "END:VTODO\r\n",
                "END:VCALENDAR\r\n",
            )
        );
    }

    #[test]
    fn the_committed_fixture_files_still_match_what_the_planner_emits() {
        // The companion of
        // `the_compatibility_fixtures_render_these_exact_bytes`, and
        // deliberately not a replacement for it. That test hand-writes the
        // expectation, which is what makes it an *independent* statement of
        // the intended bytes; this one reads the files off disk, which is
        // what makes it a *drift guard* for the artefacts actually handed to
        // caldir and vassago in `compat-report.md`. Neither subsumes the
        // other: without the literal there is no independent expectation,
        // without the file read a fixture could rot unnoticed.
        let root = Path::new("/caldir");

        let with_window = plan(
            root,
            &fixture_course(),
            &[fixture_assignment()],
            &[],
            &State::default(),
        );
        assert_eq!(
            only_vtodo(&with_window).content,
            read_fixture("deadline-full.ics"),
            "deadline-full.ics has drifted from the planner's output"
        );

        let mut no_unlock = fixture_assignment();
        no_unlock.unlock_at = None;
        let without_window = plan(
            root,
            &fixture_course(),
            &[no_unlock],
            &[],
            &State::default(),
        );
        assert_eq!(
            only_vtodo(&without_window).content,
            read_fixture("deadline-no-unlock.ics"),
            "deadline-no-unlock.ics has drifted from the planner's output"
        );
    }

    #[test]
    fn a_course_rename_rewrites_the_same_file_under_the_same_uid() {
        // `SUMMARY` and `DESCRIPTION` now depend on `Course.name` (spec ID1),
        // so a course rename is newly a content change. It must still be a
        // rewrite of the same component, not a new object: same filename,
        // same UID, same state key, and nothing planned for deletion.
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let before = plan(
            root,
            &course(),
            std::slice::from_ref(&assignment),
            &[],
            &State::default(),
        );
        let prev = state_with_hash(assignment.id, &before.writes[0].content);

        let renamed_course = Course {
            id: 1,
            name: "Intro to Testing (Section B)".into(),
            course_code: Some("TST101".into()),
        };
        let after = plan(root, &renamed_course, &[assignment], &[], &prev);

        assert_eq!(after.writes.len(), 1);
        assert_eq!(
            after.writes[0].path.file_name(),
            before.writes[0].path.file_name()
        );
        assert_eq!(after.writes[0].state_key, "calendar:555");
        assert!(after.writes[0]
            .content
            .contains("UID:u_crawler-todo-555@u-crawler.local"));
        assert_eq!(
            logical_line(&after.writes[0].content, "SUMMARY:"),
            "SUMMARY:Intro to Testing (Section B) - Essay Draft"
        );
        assert!(after.deletes.is_empty());
    }

    #[test]
    fn a_course_rename_moves_the_course_directory_and_that_predates_this_change() {
        // Honest pin of a real limitation rather than a claim it does not
        // exist. The *file* is stable, but the directory above it is
        // `canvas::course::course_dir`, derived from the course's sanitized name —
        // shared with `download_root` and with the frozen `windows`
        // collection, and unchanged by this ticket. Renaming a course in
        // Canvas therefore leaves the old course directory behind; the
        // deletion pass in `plan` reconciles assignments, not directories.
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let before = plan(
            root,
            &course(),
            std::slice::from_ref(&assignment),
            &[],
            &State::default(),
        );
        let renamed_course = Course {
            id: 1,
            name: "Intro to Testing (Section B)".into(),
            course_code: Some("TST101".into()),
        };
        let after = plan(root, &renamed_course, &[assignment], &[], &State::default());

        assert_eq!(
            before.writes[0].path,
            Path::new("/caldir/Intro_to_Testing_TST101/deadlines/assignment-555.ics")
        );
        assert_eq!(
            after.writes[0].path,
            Path::new("/caldir/Intro_to_Testing_Section_B_TST101/deadlines/assignment-555.ics")
        );
    }

    #[test]
    fn an_unlock_at_change_rewrites_both_components_in_place() {
        // `unlock_at` now feeds the `VTODO` (`DTSTART` and the availability
        // line) as well as the `VEVENT` it always fed. Moving it must rewrite
        // both existing files, not mint new ones.
        let root = Path::new("/caldir");
        let assignment = fixture_assignment();
        let before = plan(
            root,
            &fixture_course(),
            std::slice::from_ref(&assignment),
            &[],
            &State::default(),
        );
        assert_eq!(before.writes.len(), 2);
        let mut prev = State::default();
        record_writes(&mut prev, &before.writes);

        let mut moved = fixture_assignment();
        moved.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-09-10T08:00:00Z")
                .unwrap()
                .into(),
        );
        let after = plan(root, &fixture_course(), &[moved], &[], &prev);

        assert_eq!(after.writes.len(), 2);
        let mut before_paths: Vec<_> = before.writes.iter().map(|w| w.path.clone()).collect();
        let mut after_paths: Vec<_> = after.writes.iter().map(|w| w.path.clone()).collect();
        before_paths.sort();
        after_paths.sort();
        assert_eq!(before_paths, after_paths);
        assert!(after.deletes.is_empty());

        let vtodo = only_vtodo(&after);
        assert!(vtodo
            .content
            .contains("UID:u_crawler-todo-90210@u-crawler.local"));
        assert!(vtodo.content.contains("DTSTART:20260910T080000Z\r\n"));
        assert_eq!(
            logical_line(&vtodo.content, "DESCRIPTION:"),
            concat!(
                r"DESCRIPTION:Cálculo Diferencial - Sumativa 5: Informe de laboratorio",
                r"\nDisponible: 2026-09-10T08:00:00Z - Vence: 2026-09-16T23:59:00Z",
                r"\nhttps://canvas.example.edu/courses/4210/assignments/90210",
            )
        );

        let vevent = after
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VEVENT"))
            .expect("a vevent write");
        assert!(vevent
            .content
            .contains("UID:u_crawler-window-90210@u-crawler.local"));
        assert!(vevent.content.contains("DTSTART:20260910T080000Z\r\n"));
    }

    #[test]
    fn gaining_an_unlock_at_adds_a_dtstart_and_a_window_without_moving_the_deadline() {
        let root = Path::new("/caldir");
        let mut without = fixture_assignment();
        without.unlock_at = None;
        let before = plan(
            root,
            &fixture_course(),
            std::slice::from_ref(&without),
            &[],
            &State::default(),
        );
        assert_eq!(before.writes.len(), 1);
        assert!(!before.writes[0].content.contains("DTSTART:"));
        let mut prev = State::default();
        record_writes(&mut prev, &before.writes);

        let after = plan(root, &fixture_course(), &[fixture_assignment()], &[], &prev);

        // The deadline is rewritten in place (it gained a `DTSTART`), and the
        // window appears for the first time.
        assert_eq!(after.writes.len(), 2);
        assert_eq!(only_vtodo(&after).path, before.writes[0].path);
        assert!(only_vtodo(&after)
            .content
            .contains("DTSTART:20260909T140000Z\r\n"));
        assert!(after.deletes.is_empty());
    }

    #[test]
    fn one_recorded_run_converges_for_every_component_of_a_rich_assignment() {
        // The guarantee of this ticket (spec ID8, user story 15): with an
        // empty state, a fully-populated assignment costs exactly one write
        // per component; recording those writes with the module's own
        // `record_writes` and planning again against that state costs
        // nothing at all — no writes and, just as importantly, no deletes.
        //
        // Deliberately the *rich* shape: graded submittable assignment
        // (`PRIORITY:1`), a submitted submission (`STATUS:COMPLETED`), a
        // coherent window (`DTSTART` plus a `VEVENT`), and the new
        // `SUMMARY`/`DESCRIPTION`. If any of those made the render depend on
        // something other than Canvas data, the second plan would not be
        // empty.
        let root = Path::new("/caldir");
        let assignment = fixture_assignment();
        let submissions = vec![submission_submitted(assignment.id)];

        let first = plan(
            root,
            &fixture_course(),
            std::slice::from_ref(&assignment),
            &submissions,
            &State::default(),
        );
        assert_eq!(first.writes.len(), 2, "one write per component");
        assert_eq!(
            first
                .writes
                .iter()
                .map(|w| w.state_key.as_str())
                .collect::<Vec<_>>(),
            vec!["calendar:90210", "calendar-window:90210"]
        );
        assert!(only_vtodo(&first).content.contains("STATUS:COMPLETED\r\n"));

        let mut state = State::default();
        record_writes(&mut state, &first.writes);

        let second = plan(
            root,
            &fixture_course(),
            std::slice::from_ref(&assignment),
            &submissions,
            &state,
        );
        assert_eq!(second, Plan::default(), "second run must plan nothing");

        // And a third, from the same state, for the same reason: nothing
        // about the projection drifts on its own between runs.
        let third = plan(root, &fixture_course(), &[assignment], &submissions, &state);
        assert_eq!(third, Plan::default());
    }

    #[test]
    fn a_completed_rich_assignment_still_renders_its_status_and_priority() {
        // Cross-check that the properties ticket 12 and 14 inserted did not
        // displace the ones tickets 07 and 09 own: `PRIORITY` and `STATUS`
        // still sit between `DUE` and `SUMMARY`, in that order.
        let root = Path::new("/caldir");
        let assignment = fixture_assignment();
        let submissions = vec![submission_submitted(assignment.id)];
        let got = plan(
            root,
            &fixture_course(),
            &[assignment],
            &submissions,
            &State::default(),
        );

        assert!(only_vtodo(&got).content.contains(concat!(
            "DUE:20260916T235900Z\r\n",
            "PRIORITY:1\r\n",
            "STATUS:COMPLETED\r\n",
            "SUMMARY:Cálculo Diferencial - Sumativa 5: Informe de laboratorio\r\n",
        )));
    }

    #[test]
    fn a_rich_assignment_deleted_from_canvas_still_reconciles_both_components() {
        // Deletion reconciliation (spec D5, ticket 10) reads only state keys
        // and assignment ids, so the new properties cannot reach it — pinned
        // anyway, because "cannot reach it" is exactly the kind of claim that
        // stops being true quietly.
        let root = Path::new("/caldir");
        let assignment = fixture_assignment();
        let first = plan(
            root,
            &fixture_course(),
            &[assignment],
            &[],
            &State::default(),
        );
        let mut state = State::default();
        record_writes(&mut state, &first.writes);

        let got = plan(root, &fixture_course(), &[], &[], &state);

        assert!(got.writes.is_empty());
        let mut deletes: Vec<_> = got
            .deletes
            .iter()
            .map(|d| (d.path.clone(), d.state_key.clone()))
            .collect();
        deletes.sort();
        assert_eq!(
            deletes,
            vec![
                (
                    PathBuf::from(
                        "/caldir/Calculo_Diferencial_MAT1101/deadlines/assignment-90210.ics"
                    ),
                    "calendar:90210".to_string()
                ),
                (
                    PathBuf::from(
                        "/caldir/Calculo_Diferencial_MAT1101/windows/assignment-90210.ics"
                    ),
                    "calendar-window:90210".to_string()
                ),
            ]
        );
    }

    #[test]
    fn a_failed_fetch_of_a_rich_course_still_plans_nothing_at_all() {
        // `plan_for_course`'s gate (spec ticket 11) is upstream of every
        // property this feature added, and stays that way: a failed fetch is
        // `None`, never an empty plan that would read as "delete everything".
        let root = Path::new("/caldir");
        let mut state = State::default();
        let first = plan(
            root,
            &fixture_course(),
            &[fixture_assignment()],
            &[],
            &State::default(),
        );
        record_writes(&mut state, &first.writes);

        assert!(plan_for_course(
            root,
            &fixture_course(),
            Err(FetchFailed),
            Ok(vec![]),
            &state
        )
        .is_none());
        assert!(plan_for_course(
            root,
            &fixture_course(),
            Ok(vec![]),
            Err(FetchFailed),
            &state
        )
        .is_none());
    }

    // --- deletion reconciliation (ticket 10, spec D5) ---
    //
    // The three cases the ticket names by name: an assignment gone from
    // Canvas gets deleted, one still present does not, and a failed course
    // fetch must never reach `plan` disguised as an empty list. Plus: both
    // the VTODO and the VEVENT go when both existed, and `record_deletes`
    // stops the state from referencing what was just deleted.

    #[test]
    fn assignment_absent_from_canvas_deletes_both_vtodo_and_vevent() {
        let root = Path::new("/caldir");
        let deleted_id = 555;
        let mut prev = State::default();
        prev.set(
            calendar_state_key(deleted_id),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("stale-todo-hash".into()),
                last_error: None,
                error_count: None,
            },
        );
        prev.set(
            window_state_key(deleted_id),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("stale-window-hash".into()),
                last_error: None,
                error_count: None,
            },
        );

        // Canvas's current response for this course no longer mentions
        // `deleted_id` at all.
        let got = plan(root, &course(), &[], &[], &prev);

        assert!(got.writes.is_empty());
        assert_eq!(got.deletes.len(), 2);

        let todo_delete = got
            .deletes
            .iter()
            .find(|d| d.state_key == calendar_state_key(deleted_id))
            .expect("vtodo delete planned");
        assert_eq!(
            todo_delete.path,
            Path::new("/caldir/Intro_to_Testing_TST101/deadlines/assignment-555.ics")
        );
        assert_eq!(todo_delete.assignment_id, deleted_id);

        let window_delete = got
            .deletes
            .iter()
            .find(|d| d.state_key == window_state_key(deleted_id))
            .expect("vevent delete planned");
        assert_eq!(
            window_delete.path,
            Path::new("/caldir/Intro_to_Testing_TST101/windows/assignment-555.ics")
        );
        assert_eq!(window_delete.assignment_id, deleted_id);
    }

    #[test]
    fn assignment_still_present_in_canvas_is_not_deleted() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date(); // id 555
        let content = render_vtodo(
            &deadline_uid(assignment.id),
            &course(),
            &assignment,
            assignment.due_at.unwrap(),
            false,
        );
        let prev = state_with_hash(assignment.id, &content);

        let got = plan(
            root,
            &course(),
            std::slice::from_ref(&assignment),
            &[],
            &prev,
        );

        assert!(got.deletes.is_empty());
    }

    #[test]
    fn a_failed_course_fetch_never_reaches_plan_and_produces_no_deletions() {
        // Ticket 10's safety requirement, in the form the ticket asks for: a
        // course whose assignment fetch failed must not trigger a delete of
        // its previously-tracked components. `plan_for_course` is the gate
        // -- an `Err` for either fetch must short-circuit to `None` before
        // `plan`'s reconciliation pass (which would otherwise see "no
        // assignments" and delete everything the previous state remembers)
        // ever runs.
        let root = Path::new("/caldir");
        let assignment_id = 555;
        let mut prev = State::default();
        prev.set(
            calendar_state_key(assignment_id),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("stale-hash".into()),
                last_error: None,
                error_count: None,
            },
        );

        let assignments_failed: Result<Vec<Assignment>, FetchFailed> = Err(FetchFailed);
        let submissions_failed: Result<Vec<Submission>, FetchFailed> = Err(FetchFailed);

        let got = plan_for_course(
            root,
            &course(),
            assignments_failed,
            submissions_failed,
            &prev,
        );

        // No plan at all was produced -- not an empty one.
        assert!(got.is_none());

        // The state entry from before is left completely untouched: it
        // still compares as "the same stale hash it always was", which is
        // exactly what must be true after a failure.
        assert_eq!(
            prev.get(&calendar_state_key(assignment_id))
                .and_then(|i| i.content_hash.as_deref()),
            Some("stale-hash")
        );
    }

    #[test]
    fn assignments_fetch_ok_but_submissions_fetch_failed_also_produces_no_plan() {
        // Both fetches must succeed, not just the first one -- a submissions
        // failure is exactly as dangerous as an assignments failure, since
        // `plan` needs both to render a `VTODO`'s STATUS correctly.
        let root = Path::new("/caldir");
        let assignments_ok: Result<Vec<Assignment>, FetchFailed> =
            Ok(vec![assignment_with_due_date()]);
        let submissions_failed: Result<Vec<Submission>, FetchFailed> = Err(FetchFailed);

        let got = plan_for_course(
            root,
            &course(),
            assignments_ok,
            submissions_failed,
            &State::default(),
        );

        assert!(got.is_none());
    }

    #[test]
    fn both_fetches_succeeding_produces_a_plan() {
        let root = Path::new("/caldir");
        let assignments_ok: Result<Vec<Assignment>, FetchFailed> =
            Ok(vec![assignment_with_due_date()]);
        let submissions_ok: Result<Vec<Submission>, FetchFailed> = Ok(vec![]);

        let got = plan_for_course(
            root,
            &course(),
            assignments_ok,
            submissions_ok,
            &State::default(),
        )
        .expect("both fetches succeeded");

        assert_eq!(got.writes.len(), 1);
    }

    #[test]
    fn an_unrelated_state_key_with_the_calendar_prefix_as_a_substring_is_not_touched() {
        // `state.json` is shared with `canvas/sync.rs`, which uses its own
        // `assignment:{id}` namespace for unrelated bookkeeping. This pins
        // that the reconciliation scan only matches the real
        // `calendar:`/`calendar-window:` prefixes, not anything that merely
        // contains "calendar" as a substring elsewhere in the key.
        let root = Path::new("/caldir");
        let mut prev = State::default();
        prev.set(
            "assignment:555".to_string(),
            ItemState {
                etag: None,
                updated_at: None,
                size: None,
                content_hash: Some("unrelated".into()),
                last_error: None,
                error_count: None,
            },
        );

        let got = plan(root, &course(), &[], &[], &prev);

        assert!(got.deletes.is_empty());
    }
}
