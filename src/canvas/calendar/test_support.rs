//! Fixtures shared by the calendar module's test modules.

use std::path::{Path, PathBuf};

use chrono::DateTime;
use sha1::{Digest, Sha1};

use super::{Plan, PlannedWrite};
use crate::canvas::{Assignment, Course, Submission};
use crate::core::state::{ItemState, State};

pub(super) fn course() -> Course {
    Course {
        id: 1,
        name: "Intro to Testing".into(),
        course_code: Some("TST101".into()),
    }
}

pub(super) fn assignment_without_due_date() -> Assignment {
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

pub(super) fn assignment_with_due_date() -> Assignment {
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

/// Undo RFC 5545 §3.1 line folding, so a test can assert against one
/// logical content line. This is the *inverse* of what the renderer
/// does, not a copy of it: it never composes an expected value, it only
/// makes the produced value readable. Every expected string in these
/// tests is still a hand-written literal.
pub(super) fn unfold(content: &str) -> Vec<String> {
    let mut logical: Vec<String> = Vec::new();
    for raw in content.split("\r\n") {
        match raw.strip_prefix(' ') {
            Some(rest) => match logical.last_mut() {
                Some(last) => last.push_str(rest),
                None => logical.push(rest.to_string()),
            },
            None => logical.push(raw.to_string()),
        }
    }
    logical
}

/// The single logical content line of `content` starting with `prefix`.
pub(super) fn logical_line(content: &str, prefix: &str) -> String {
    let matches: Vec<String> = unfold(content)
        .into_iter()
        .filter(|line| line.starts_with(prefix))
        .collect();
    assert_eq!(matches.len(), 1, "expected exactly one {prefix} line");
    matches.into_iter().next().expect("one match")
}

/// Build a `State` recording, under the `calendar:{id}` namespace, a
/// content hash computed independently of `plan`'s own `content_hash`
/// helper (using the `sha1` crate directly, the same way `canvas/sync.rs`
/// does), so this does not just recompute the production code's answer
/// and compare it to itself.
pub(super) fn state_with_hash(assignment_id: u64, content: &str) -> State {
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

/// The course used by the compatibility fixtures: a real-shaped Spanish
/// name, so the accented octet is inside the folded region rather than
/// only in a synthetic `"ñ".repeat(50)`.
pub(super) fn fixture_course() -> Course {
    Course {
        id: 4210,
        name: "Cálculo Diferencial".into(),
        course_code: Some("MAT1101".into()),
    }
}

/// The assignment used by the compatibility fixtures: course label, a
/// coherent availability window, a URL, and a graded, submittable
/// assignment so `PRIORITY:1` is exercised alongside the new fields.
pub(super) fn fixture_assignment() -> Assignment {
    Assignment {
        id: 90210,
        name: Some("Sumativa 5: Informe de laboratorio".into()),
        description: None,
        updated_at: Some("2026-09-01T18:30:00Z".into()),
        due_at: Some(
            DateTime::parse_from_rfc3339("2026-09-16T23:59:00Z")
                .unwrap()
                .into(),
        ),
        unlock_at: Some(
            DateTime::parse_from_rfc3339("2026-09-09T14:00:00Z")
                .unwrap()
                .into(),
        ),
        lock_at: None,
        points_possible: Some(30.0),
        omit_from_final_grade: Some(false),
        html_url: Some("https://canvas.example.edu/courses/4210/assignments/90210".into()),
        assignment_group_id: None,
        submission_types: Some(vec!["online_upload".into()]),
        published: None,
    }
}

/// The deadline `VTODO` of a plan, or a panic naming what was there
/// instead. Reading, never composing.
pub(super) fn only_vtodo(got: &Plan) -> &PlannedWrite {
    got.writes
        .iter()
        .find(|w| w.content.contains("BEGIN:VTODO"))
        .expect("a vtodo write")
}

/// The two `.ics` files committed under
/// `.scratch/calendar-rich-vtodo/fixtures/`, resolved from
/// `CARGO_MANIFEST_DIR` so the test does not care what the process
/// working directory is.
pub(super) fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".scratch/calendar-rich-vtodo/fixtures")
        .join(name)
}

/// The bytes of a committed fixture, or a panic naming the path. Never a
/// skip: a fixture that has gone missing is the exact drift this test
/// exists to catch, so its absence must be as loud as a mismatch.
pub(super) fn read_fixture(name: &str) -> String {
    let path = fixture_path(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("committed fixture {} is unreadable: {e}", path.display()))
}

pub(super) fn submission_submitted(assignment_id: u64) -> Submission {
    Submission {
        assignment_id,
        submitted_at: Some(
            DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z")
                .unwrap()
                .into(),
        ),
        workflow_state: Some("submitted".into()),
    }
}

pub(super) fn submission_graded_without_submitted_at(assignment_id: u64) -> Submission {
    Submission {
        assignment_id,
        submitted_at: None,
        workflow_state: Some("graded".into()),
    }
}

pub(super) fn submission_untouched(assignment_id: u64) -> Submission {
    Submission {
        assignment_id,
        submitted_at: None,
        workflow_state: Some("unsubmitted".into()),
    }
}

pub(super) fn course_with(id: u64, name: &str) -> Course {
    Course {
        id,
        name: name.into(),
        course_code: None,
    }
}
