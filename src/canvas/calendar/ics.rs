//! iCalendar primitives shared by both renderers: UTC `DATE-TIME` formatting,
//! TEXT escaping, `DTSTAMP` derivation, the availability-window predicate and
//! RFC 5545 §3.1 line folding.

use chrono::{DateTime, Utc};

use crate::canvas::Assignment;

/// Format a UTC instant the way iCalendar wants it: `Z`-suffixed, no offset
/// arithmetic required by the reader (spec D9).
pub(super) fn ics_datetime(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Escape iCalendar TEXT value special characters (RFC 5545 §3.3.11).
pub(super) fn escape_text(s: &str) -> String {
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
pub(super) fn dtstamp_for(assignment: &Assignment, due: DateTime<Utc>) -> DateTime<Utc> {
    assignment
        .updated_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(due)
}

/// The instant an assignment becomes available, but only when Canvas's data
/// describes a window coherent enough to represent: `unlock_at` present *and*
/// **strictly** before `due_at`.
///
/// The inequality is strict because RFC 5545 §3.8.2.3 makes it so — the value
/// of `DUE` "MUST be later in time than the value of the 'DTSTART' property".
/// "Later in time than", not "no earlier than": an `unlock_at` equal to
/// `due_at` breaks that MUST exactly as an inverted one does. §3.6.2 lists
/// `dtstart` as OPTIONAL, so dropping the property is always legal, which
/// makes `None` the branch that cannot turn inconsistent Canvas data into an
/// invalid component.
///
/// One predicate, two callers, deliberately: [`render_vtodo`](super::vtodo::render_vtodo) asks it whether
/// the deadline may carry a `DTSTART` (spec ID4) and [`plan`](fn@super::plan::plan) asks it whether
/// an availability-window `VEVENT` exists at all (spec D3). Sharing it is what
/// keeps the two collections from ever disagreeing about whether an
/// assignment has a window — the incoherent case yields a `VTODO` without
/// `DTSTART` *and* no `VEVENT`, never one without the other.
pub(super) fn availability_start(
    unlock_at: Option<DateTime<Utc>>,
    due: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    unlock_at.filter(|unlock| *unlock < due)
}

/// The RFC 5545 §3.1 ceiling for a content line: "Lines of text SHOULD NOT be
/// longer than 75 octets, excluding the line break." Octets, not characters.
const MAX_LINE_OCTETS: usize = 75;

/// Fold one content line per RFC 5545 §3.1: split it into chunks of at most
/// [`MAX_LINE_OCTETS`] octets joined by `CRLF` plus a single SPACE, which the
/// reader's mandatory unfolding step removes again.
///
/// The continuation's leading SPACE counts against the 75, so a continuation
/// carries at most 74 octets of value. Exactly one SPACE is inserted and no
/// more: §3.1's own example shows a second space surviving unfolding as part
/// of the value, which for a URL would be corruption.
///
/// Splits only on character boundaries. §3.1's note calls a fold made inside
/// a UTF-8 multi-octet sequence "improperly folded"; with Spanish course
/// names (`á`, `ñ`, `¿`, em dashes) a naive octet cut would hit one routinely.
/// The longest UTF-8 sequence is 4 octets and the smallest budget here is 74,
/// so backing up to a boundary always leaves progress to make and the loop
/// always terminates.
pub(super) fn fold_line(line: &str) -> String {
    if line.len() <= MAX_LINE_OCTETS {
        return line.to_string();
    }
    let mut folded = String::with_capacity(line.len() + line.len() / MAX_LINE_OCTETS * 3);
    let mut start = 0;
    // The first line spends nothing on a continuation SPACE; every later one
    // spends exactly one octet on it.
    let mut budget = MAX_LINE_OCTETS;
    while line.len() - start > budget {
        let mut end = start + budget;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        folded.push_str(&line[start..end]);
        folded.push_str("\r\n ");
        start = end;
        budget = MAX_LINE_OCTETS - 1;
    }
    folded.push_str(&line[start..]);
    folded
}

#[cfg(test)]
mod tests {
    use crate::canvas::calendar::plan::plan;
    use crate::canvas::calendar::test_support::{assignment_with_due_date, course, logical_line};
    use crate::canvas::Course;
    use crate::core::state::State;
    use std::path::Path;

    #[test]
    fn long_vtodo_lines_are_folded_at_75_octets_on_character_boundaries() {
        // RFC 5545 §3.1: "Lines of text SHOULD NOT be longer than 75 octets,
        // excluding the line break", and a fold is CRLF plus one SPACE. The
        // §3.1 note calls a fold inside a multi-octet UTF-8 sequence
        // "improperly folded", which matters the moment a course is named in
        // Spanish.
        let root = Path::new("/caldir");
        // Each 'ñ' is two octets, so a naive 75-octet cut would land inside
        // one: "SUMMARY:" is 8 octets, leaving 67 for the value, and 67 is
        // odd. The last whole character that fits is the 33rd.
        let long_name = "ñ".repeat(50);
        let long_course = Course {
            id: 1,
            name: long_name.clone(),
            course_code: Some("MAT101".into()),
        };
        let mut assignment = assignment_with_due_date();
        assignment.name = None;

        let got = plan(root, &long_course, &[assignment], &[], &State::default());
        let content = &got.writes[0].content;

        for line in content.split("\r\n") {
            assert!(
                line.len() <= 75,
                "line of {} octets exceeds the RFC 5545 §3.1 limit: {line:?}",
                line.len()
            );
        }

        // 33 characters of value on the first line (8 + 66 = 74 octets), the
        // remaining 17 on a continuation opened by exactly one SPACE.
        let expected_fold = format!("SUMMARY:{}\r\n {}\r\n", "ñ".repeat(33), "ñ".repeat(17));
        assert!(
            content.contains(&expected_fold),
            "expected fold not found in {content:?}"
        );

        // Unfolding restores the value byte for byte — nothing was lost or
        // duplicated at the seam.
        assert_eq!(
            logical_line(content, "SUMMARY:"),
            format!("SUMMARY:{long_name}")
        );
    }

    #[test]
    fn dtstamp_is_derived_from_updated_at_when_present_and_parseable() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.updated_at = Some("2026-08-20T10:15:00Z".into());
        let got = plan(root, &course(), &[assignment], &[], &State::default());
        assert!(got.writes[0].content.contains("DTSTAMP:20260820T101500Z"));
    }

    #[test]
    fn dtstamp_falls_back_to_due_at_when_updated_at_is_absent() {
        let root = Path::new("/caldir");
        let assignment = assignment_with_due_date();
        assert!(assignment.updated_at.is_none());
        let got = plan(root, &course(), &[assignment], &[], &State::default());
        // due_at for assignment_with_due_date() is 2026-09-01T23:59:00Z
        assert!(got.writes[0].content.contains("DTSTAMP:20260901T235900Z"));
    }

    #[test]
    fn dtstamp_falls_back_to_due_at_when_updated_at_does_not_parse() {
        let root = Path::new("/caldir");
        let mut assignment = assignment_with_due_date();
        assignment.updated_at = Some("not-a-date".into());
        let got = plan(root, &course(), &[assignment], &[], &State::default());
        assert!(got.writes[0].content.contains("DTSTAMP:20260901T235900Z"));
    }
}
