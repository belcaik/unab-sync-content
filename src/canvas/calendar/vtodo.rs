//! The deadline `VTODO` renderer: priority from Canvas grading state (spec
//! D6, ticket 07), completion status from the caller's submissions (spec D7,
//! ticket 09), the human-readable label and description (spec ID1–ID3), CR
//! normalization and RFC 5545 §3.1 folding.

use chrono::{DateTime, SecondsFormat, Utc};

use super::ics::{availability_start, dtstamp_for, escape_text, fold_line, ics_datetime};
use crate::canvas::{Assignment, Course, Submission};

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

/// Whether a submission counts as "done" for `STATUS:COMPLETED` purposes
/// (spec D7, ticket 09): `submitted_at` is present, or `workflow_state` is
/// `"graded"` — grading without a recorded submission timestamp still counts,
/// since a teacher can grade an in-person or verbally-assessed piece of work
/// that Canvas never saw a file for.
///
/// Deliberately reads only these two fields. In particular it does not special
/// -case group assignments: Canvas already propagates a group member's
/// submission onto every teammate's own submission record, so the caller's
/// own `students/submissions?student_ids[]=self` response reflects a
/// groupmate's turn-in without this function needing to know groups exist.
pub(super) fn is_submission_done(submission: &Submission) -> bool {
    submission.submitted_at.is_some() || submission.workflow_state.as_deref() == Some("graded")
}

/// Collapse every flavour of line ending Canvas can hand us (`\r\n`, lone
/// `\r`) down to a bare `\n`, so the escaper downstream sees exactly one
/// representation of "line break".
///
/// This lives here and **not** inside [`escape_text`](super::ics::escape_text) on purpose. `escape_text`
/// is shared with [`render_vevent`](super::vevent::render_vevent), whose bytes this ticket must not move;
/// widening it would change the `windows` collection too. The `VTODO` text
/// path is the only place that needs the normalization, so it is the only
/// place that gets it.
///
/// Why it is needed at all: a lone `\r` survives [`escape_text`](super::ics::escape_text) untouched and
/// reaches the published file verbatim. vassago's `unfold`
/// (`merge-ucrawler.py:21`) treats a lone `\r` as a line terminator, so that
/// stray octet silently splits one property into two on the way through the
/// bridge. RFC 5545 §3.1 excludes CR from `VALUE-CHAR` as well
/// (`CONTROL = %x00-08 / %x0A-1F / %x7F`), so emitting one was never
/// conformant to begin with.
fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// Prepare a Canvas-sourced string for use as an RFC 5545 TEXT value in the
/// `VTODO`: normalize line endings first ([`normalize_newlines`]), then apply
/// the §3.3.11 escaping ([`escape_text`](super::ics::escape_text)). Order matters — normalizing after
/// escaping would leave the `\r` behind the `\n` that was already turned into
/// a `\n` escape.
fn vtodo_text(s: &str) -> String {
    escape_text(&normalize_newlines(s))
}

/// Format an instant for the *human-readable* body of the `DESCRIPTION`:
/// RFC 3339, UTC, whole seconds — `2026-09-09T14:00:00Z` (spec ID3).
///
/// Deliberately not [`ics_datetime`](super::ics::ics_datetime)'s `20260909T140000Z`: that form is for
/// property values a machine reads, this one is read by a student inside a
/// task's notes. It is UTC rather than a local zone because the project has
/// no presentation timezone and this change does not invent one (spec D9,
/// and AGENTS.md's ban on inert configuration). It also earns its keep: the
/// `DUE` that reaches Google Tasks is reduced to a bare day, so this line is
/// the only place the exact hour survives the pipeline.
fn rfc3339_utc(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// **Returns an already-escaped property value — never pass the result through
/// [`vtodo_text`] again, or every `\` and `,` in it would be escaped twice.**
/// Each logical line is escaped internally, on its way in.
///
/// Build the `DESCRIPTION` value of a deadline `VTODO` (spec ID2): three
/// logical lines, joined by the RFC 5545 §3.3.11 `\n` escape (a backslash and
/// an `n`, two octets in the file — not a real line break).
///
/// 1. the [`deadline_label_raw`], the same text `SUMMARY` carries;
/// 2. `Disponible: <unlock_at> - Vence: <due_at>`, where a genuinely absent
///    `unlock_at` reads `sin fecha de apertura` rather than a fabricated
///    date;
/// 3. the assignment's `html_url` — **omitted entirely** when Canvas did not
///    give one, rather than emitted empty or padded.
///
/// The URL is repeated here even though the component already carries a
/// `URL` property, and that is deliberate rather than redundant: `caldir`
/// never forwards `URL` to Google Tasks (`to_google.rs:17-45`), so
/// `DESCRIPTION` — which it maps to the task's `notes` — is the only route by
/// which the link reaches the place the student reads.
///
/// `assignment.description` (Canvas's HTML body) is deliberately absent. It is
/// exactly the large HTML-derived blob that makes vassago's `shared_signature`
/// diverge when Google normalizes `notes`, producing a sticky `CONFLICT` that
/// blocks publication. Short, plain and stable is the mitigation.
///
/// Line 2 always exists — this is only ever called for an assignment that has
/// a `due_at` — so the value can never come out empty and there is no
/// "omit the whole property" case to handle.
fn deadline_description_escaped(
    course: &Course,
    assignment: &Assignment,
    due: DateTime<Utc>,
) -> String {
    let available = match assignment.unlock_at {
        Some(unlock) => rfc3339_utc(unlock),
        None => "sin fecha de apertura".to_string(),
    };
    let mut logical_lines = vec![
        vtodo_text(&deadline_label_raw(course, assignment)),
        vtodo_text(&format!(
            "Disponible: {available} - Vence: {}",
            rfc3339_utc(due)
        )),
    ];
    if let Some(url) = &assignment.html_url {
        logical_lines.push(vtodo_text(url));
    }
    logical_lines.join("\\n")
}

/// **Returns raw, unescaped text — the caller MUST run it through
/// [`vtodo_text`] before it becomes a property value.** The `_raw` suffix is
/// the whole point of the name: its sibling
/// [`deadline_description_escaped`] escapes internally, this one does not,
/// and the two sit one line apart in [`render_vtodo`].
///
/// The human-readable label for a deadline: `<course name> - <assignment
/// title>` (spec ID1).
///
/// The course name is [`Course::name`] — the human name Canvas shows — and
/// deliberately neither [`course_dir`](crate::canvas::course::course_dir)'s output (sanitized and
/// transliterated to ASCII) nor `course_code`. A student reading a list that
/// aggregates every course needs the name they recognize, not a path
/// component.
///
/// Only non-empty parts are joined, so a nameless assignment yields the bare
/// course name rather than `"Course - "`, and a nameless course yields the
/// bare title rather than `" - Title"`. A decorative dangling dash would be
/// noise the data never justified.
///
/// One function, two call sites: `SUMMARY` and the first logical line of
/// `DESCRIPTION` both render this, so the two can never drift apart.
fn deadline_label_raw(course: &Course, assignment: &Assignment) -> String {
    [
        course.name.as_str(),
        assignment.name.as_deref().unwrap_or(""),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" - ")
}

/// Render a single assignment deadline as a complete `.ics` file: one
/// `VCALENDAR` wrapping one `VTODO`.
///
/// `done` (spec D7, ticket 09) controls `STATUS`: when `true`, a
/// `STATUS:COMPLETED` line is emitted so the component shows as finished
/// without being deleted — the record of what was done must remain visible.
/// When `false`, **no `STATUS` line is emitted at all**, rather than an
/// explicit `STATUS:NEEDS-ACTION`. Two reasons, both about not asserting more
/// than Canvas told us:
///
/// - RFC 5545 §3.8.1.11 already treats an absent `STATUS` on a `VTODO` as
///   "needs action" — omitting it says exactly the same thing as spelling it
///   out, so there is nothing to gain from the explicit form.
/// - Emitting `NEEDS-ACTION` would be an active claim that this item is *not*
///   done, sourced from nothing but "Canvas has no opinion". Omission makes
///   the same silence structurally visible in the content itself: this
///   component carries no completion assertion one way or the other.
///
/// This also keeps the projection stable in the sense [`plan`](fn@super::plan::plan)'s content-hash
/// comparison (spec D5) needs: for a given assignment/submission pair the
/// rendered content is a pure function of that data, so an unrelated field
/// changing produces a diff exactly where the data changed and nowhere else.
pub(super) fn render_vtodo(
    uid: &str,
    course: &Course,
    assignment: &Assignment,
    due: DateTime<Utc>,
    done: bool,
) -> String {
    let dtstamp = dtstamp_for(assignment, due);
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//u_crawler//calendar-sync//EN".to_string(),
        "BEGIN:VTODO".to_string(),
        format!("UID:{uid}"),
        format!("DTSTAMP:{}", ics_datetime(dtstamp)),
    ];
    // `DTSTART` = `unlock_at`, and only when [`availability_start`] says the
    // pair is legal (spec ID4, RFC 5545 §3.8.2.3). It is serialized with the
    // very same [`ics_datetime`] as `DUE`, so both are `DATE-TIME` UTC with a
    // `Z` — §3.8.2.3 requires the two value types to match as firmly as it
    // requires the ordering. It sits ahead of `DUE`, the order §3.6.2's own
    // `VTODO` example uses.
    if let Some(unlock) = availability_start(assignment.unlock_at, due) {
        lines.push(format!("DTSTART:{}", ics_datetime(unlock)));
    }
    lines.push(format!("DUE:{}", ics_datetime(due)));
    lines.push(format!("PRIORITY:{}", priority_for(assignment)));
    if done {
        lines.push("STATUS:COMPLETED".to_string());
    }
    // The two neighbours escape at different depths, and the `_raw` /
    // `_escaped` suffixes are the only thing that says so: the label comes out
    // raw and is escaped here, the description escapes each of its logical
    // lines itself and must not be escaped a second time.
    lines.push(format!(
        "SUMMARY:{}",
        vtodo_text(&deadline_label_raw(course, assignment))
    ));
    lines.push(format!(
        "DESCRIPTION:{}",
        deadline_description_escaped(course, assignment, due)
    ));
    if let Some(url) = &assignment.html_url {
        lines.push(format!("URL:{}", escape_text(url)));
    }
    lines.push("END:VTODO".to_string());
    lines.push("END:VCALENDAR".to_string());
    // Folding is applied here and nowhere else (spec ID7): `render_vevent`
    // must keep emitting the same bytes it always has, so the `windows`
    // collection stays untouched by this ticket.
    lines
        .iter()
        .map(|line| fold_line(line))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

#[cfg(test)]
mod tests {
    use crate::canvas::calendar::plan::plan;
    use crate::canvas::calendar::test_support::{
        assignment_with_due_date, course, fixed_now, logical_line,
        submission_graded_without_submitted_at, submission_submitted, submission_untouched,
    };
    use crate::canvas::Course;
    use crate::core::state::State;
    use chrono::DateTime;
    use std::path::Path;

    #[test]
    fn the_vtodo_carries_a_three_line_description() {
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

        let vtodo = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VTODO"))
            .expect("vtodo present");

        // Hand-written literal (spec ID2/ID3): label, availability line with
        // RFC 3339 UTC instants, then the assignment URL. `\n` here is the
        // RFC 5545 §3.3.11 escape, two characters in the file.
        assert_eq!(
            logical_line(&vtodo.content, "DESCRIPTION:"),
            "DESCRIPTION:Intro to Testing - Essay Draft\\nDisponible: 2026-08-01T00:00:00Z - Vence: 2026-09-01T23:59:00Z\\nhttps://canvas.example.edu/courses/1/assignments/555"
        );
    }

    #[test]
    fn a_missing_unlock_at_says_so_instead_of_inventing_a_date() {
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

        assert_eq!(
            logical_line(&got.writes[0].content, "DESCRIPTION:"),
            "DESCRIPTION:Intro to Testing - Essay Draft\\nDisponible: sin fecha de apertura - Vence: 2026-09-01T23:59:00Z\\nhttps://canvas.example.edu/courses/1/assignments/555"
        );
    }

    #[test]
    fn an_unlock_at_at_or_after_the_due_at_is_still_reported_verbatim() {
        // Spec ID5: the availability line reports what Canvas said. Only a
        // genuinely absent `unlock_at` reads "sin fecha de apertura" — an
        // inconsistent one is not silently rewritten into it. (Whether such
        // an `unlock_at` earns a `DTSTART` is ticket #14's question, not
        // this line's.)
        let root = Path::new("/caldir");
        let mut same_instant = assignment_with_due_date();
        same_instant.unlock_at = same_instant.due_at;
        let got = plan(
            root,
            &course(),
            &[same_instant],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert_eq!(
            logical_line(&got.writes[0].content, "DESCRIPTION:"),
            "DESCRIPTION:Intro to Testing - Essay Draft\\nDisponible: 2026-09-01T23:59:00Z - Vence: 2026-09-01T23:59:00Z\\nhttps://canvas.example.edu/courses/1/assignments/555"
        );

        let mut after = assignment_with_due_date();
        after.unlock_at = Some(
            DateTime::parse_from_rfc3339("2026-09-15T00:00:00Z")
                .unwrap()
                .into(),
        );
        let got = plan(
            root,
            &course(),
            &[after],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert_eq!(
            logical_line(&got.writes[0].content, "DESCRIPTION:"),
            "DESCRIPTION:Intro to Testing - Essay Draft\\nDisponible: 2026-09-15T00:00:00Z - Vence: 2026-09-01T23:59:00Z\\nhttps://canvas.example.edu/courses/1/assignments/555"
        );
    }

    #[test]
    fn a_missing_html_url_drops_the_third_description_line_entirely() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.html_url = None;
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        // Two logical lines, no trailing separator and no filler text.
        assert_eq!(
            logical_line(&got.writes[0].content, "DESCRIPTION:"),
            "DESCRIPTION:Intro to Testing - Essay Draft\\nDisponible: sin fecha de apertura - Vence: 2026-09-01T23:59:00Z"
        );
        assert!(!got.writes[0].content.contains("URL:"));
    }

    #[test]
    fn special_characters_are_escaped_and_carriage_returns_normalized_away() {
        // RFC 5545 §3.3.11: `\`, `;` and `,` are escaped, a line break
        // becomes `\n`, and `:` is left alone. Spec ID6: `\r\n` and lone `\r`
        // collapse to `\n` *before* escaping, since vassago's `unfold` treats
        // a surviving lone `\r` as a line terminator and splits the property.
        let root = Path::new("/caldir");
        let messy_course = Course {
            id: 1,
            name: r"Cálculo, Álgebra; Nivel\Avanzado".into(),
            course_code: Some("MAT101".into()),
        };
        let mut assignment = assignment_with_due_date();
        assignment.name = Some("Tarea 1\r\nParte 2\rParte 3".into());

        let got = plan(
            root,
            &messy_course,
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        let content = &got.writes[0].content;

        assert_eq!(
            logical_line(content, "SUMMARY:"),
            r"SUMMARY:Cálculo\, Álgebra\; Nivel\\Avanzado - Tarea 1\nParte 2\nParte 3"
        );
        assert_eq!(
            logical_line(content, "DESCRIPTION:"),
            r"DESCRIPTION:Cálculo\, Álgebra\; Nivel\\Avanzado - Tarea 1\nParte 2\nParte 3\nDisponible: sin fecha de apertura - Vence: 2026-09-01T23:59:00Z\nhttps://canvas.example.edu/courses/1/assignments/555"
        );

        // No stray CR survives inside any content line: the only carriage
        // returns left in the file are the CRLF line terminators themselves.
        for line in content.split("\r\n") {
            assert!(!line.contains('\r'), "stray CR in {line:?}");
        }
    }

    // --- DTSTART on the deadline VTODO (ticket 14, spec ID4) ---
    //
    // RFC 5545 §3.8.2.3: `DUE` "MUST be later in time than the value of the
    // 'DTSTART' property". Strictly later — so an `unlock_at` equal to
    // `due_at` is as illegal a pairing as an inverted one, and both branches
    // drop the property rather than the component.

    #[test]
    fn unlock_at_strictly_before_due_at_becomes_the_vtodo_dtstart() {
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
        let vtodo = got
            .writes
            .iter()
            .find(|w| w.content.contains("BEGIN:VTODO"))
            .expect("vtodo present");

        // Hand-written literals. Both are `DATE-TIME` UTC with the `Z`
        // suffix, which is also what §3.8.2.3 demands of the pair: the two
        // value types must match.
        assert_eq!(
            logical_line(&vtodo.content, "DTSTART:"),
            "DTSTART:20260801T000000Z"
        );
        assert_eq!(logical_line(&vtodo.content, "DUE:"), "DUE:20260901T235900Z");
    }

    #[test]
    fn a_missing_unlock_at_produces_no_dtstart_at_all() {
        // Canvas said nothing about when this opens, so the component says
        // nothing either — a fabricated `DTSTART` would be an assertion the
        // data never supported (spec user story 7).
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
        let content = &got.writes[0].content;
        assert!(
            !content.contains("DTSTART"),
            "no DTSTART expected in {content:?}"
        );
        assert_eq!(logical_line(content, "DUE:"), "DUE:20260901T235900Z");
    }

    #[test]
    fn an_unlock_at_equal_to_the_due_at_produces_no_dtstart_and_no_vevent() {
        // §3.8.2.3 wants `DUE` *later* than `DTSTART`; equal timestamps are
        // not later. The property goes, the component stays — and `plan`
        // already refuses the zero-length window, so both collections agree.
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.unlock_at = assignment.due_at;
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        assert_eq!(got.writes.len(), 1);
        let content = &got.writes[0].content;
        assert!(content.contains("BEGIN:VTODO"));
        assert!(
            !content.contains("DTSTART"),
            "no DTSTART expected in {content:?}"
        );
        assert_eq!(logical_line(content, "DUE:"), "DUE:20260901T235900Z");
        assert!(!got
            .writes
            .iter()
            .any(|w| w.content.contains("BEGIN:VEVENT")));
    }

    #[test]
    fn an_unlock_at_after_the_due_at_produces_no_dtstart_and_no_vevent() {
        // The inverted case, which the equal case is deliberately treated as
        // an instance of.
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        // due_at for assignment_with_due_date() is 2026-09-01T23:59:00Z.
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
        let content = &got.writes[0].content;
        assert!(content.contains("BEGIN:VTODO"));
        assert!(
            !content.contains("DTSTART"),
            "no DTSTART expected in {content:?}"
        );
        assert_eq!(logical_line(content, "DUE:"), "DUE:20260901T235900Z");
        assert!(!got
            .writes
            .iter()
            .any(|w| w.content.contains("BEGIN:VEVENT")));
    }

    #[test]
    fn an_assignment_without_a_title_summarises_as_the_bare_course_name() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.name = None;
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );

        // No dangling " - " with nothing after it (spec ID1).
        assert!(got.writes[0]
            .content
            .contains("SUMMARY:Intro to Testing\r\n"));
    }

    #[test]
    fn a_course_without_a_name_summarises_as_the_bare_assignment_title() {
        let root = Path::new("/caldir");
        let nameless = Course {
            id: 1,
            name: String::new(),
            course_code: Some("TST101".into()),
        };
        let got = plan(
            root,
            &nameless,
            &[assignment_with_due_date()],
            &[],
            fixed_now(),
            &State::default(),
        );

        // No leading " - " (spec ID1).
        assert!(got.writes[0].content.contains("SUMMARY:Essay Draft\r\n"));
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

    // --- submission/completed status (ticket 09, spec D7) ---
    //
    // The three states the ticket names, asserted through the Plan's
    // rendered content (seam S3), never by calling `is_submission_done`
    // directly.

    #[test]
    fn submission_with_submitted_at_marks_the_vtodo_completed() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let submissions = vec![submission_submitted(assignment.id)];
        let got = plan(
            root,
            &course(),
            &[assignment],
            &submissions,
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.writes.len(), 1);
        assert!(got.writes[0].content.contains("STATUS:COMPLETED"));
    }

    #[test]
    fn submission_graded_without_submitted_at_marks_the_vtodo_completed() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let submissions = vec![submission_graded_without_submitted_at(assignment.id)];
        let got = plan(
            root,
            &course(),
            &[assignment],
            &submissions,
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.writes.len(), 1);
        assert!(got.writes[0].content.contains("STATUS:COMPLETED"));
    }

    #[test]
    fn submission_neither_submitted_nor_graded_stays_pending() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let submissions = vec![submission_untouched(assignment.id)];
        let got = plan(
            root,
            &course(),
            &[assignment],
            &submissions,
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.writes.len(), 1);
        assert!(!got.writes[0].content.contains("STATUS:COMPLETED"));
        assert!(!got.writes[0].content.contains("STATUS:"));
    }

    #[test]
    fn no_matching_submission_stays_pending() {
        // No submission record at all for this assignment id -- must not be
        // confused with "done".
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.writes.len(), 1);
        assert!(!got.writes[0].content.contains("STATUS:"));
    }

    #[test]
    fn submission_for_a_different_assignment_does_not_leak_completion() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        let submissions = vec![submission_submitted(assignment.id + 1)];
        let got = plan(
            root,
            &course(),
            &[assignment],
            &submissions,
            fixed_now(),
            &State::default(),
        );
        assert_eq!(got.writes.len(), 1);
        assert!(!got.writes[0].content.contains("STATUS:"));
    }

    #[test]
    fn identical_pending_input_produces_byte_identical_content_across_runs() {
        // The stability property the pending-STATUS decision rests on: same
        // assignment, same (absent) submission state, twice -> identical
        // bytes, so an unrelated later run with the same input plans nothing
        // (spec user story 14).
        let root = Path::new("/caldir");
        let a1 = assignment_with_due_date();
        let a2 = assignment_with_due_date();
        let before = plan(root, &course(), &[a1], &[], fixed_now(), &State::default());
        let after = plan(root, &course(), &[a2], &[], fixed_now(), &State::default());
        assert_eq!(before.writes[0].content, after.writes[0].content);
    }
}
