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
//! 08), marked `STATUS:COMPLETED` when the caller's own submission for that
//! assignment is submitted or graded (spec D7, ticket 09).
//!
//! The deadline `VTODO` is written for a human to read in an aggregated task
//! list (`.scratch/calendar-rich-vtodo/spec.md`, ID1-ID3, ID6-ID7): its
//! `SUMMARY` and the first line of its `DESCRIPTION` share one formatter,
//! [`deadline_label`], so the course name and assignment title can never
//! disagree between the two; [`deadline_description`] adds the availability
//! window and the assignment link as plain text, because `caldir` forwards
//! neither `URL` nor `DTSTART` to Google Tasks and `DESCRIPTION` is the only
//! free-text field that survives the trip. Text values on this path go
//! through [`vtodo_text`] (CR normalization then RFC 5545 §3.3.11 escaping)
//! and every line through [`fold_line`] (§3.1, 75 octets). Both apply to the
//! `VTODO` only: [`render_vevent`] and the whole `windows` collection are out
//! of scope and emit exactly the bytes they always have.
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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use sha1::{Digest, Sha1};

use crate::canvas::{Assignment, CanvasClient, Course, Submission};
use crate::config::Config;
use crate::fsutil;
use crate::state::{ItemState, State};
use crate::status;

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

/// One calendar file to delete, in full (spec D5, ticket 10): the assignment
/// it belongs to no longer appears in Canvas's response, so its projected
/// component must stop existing on disk — `caldir push` propagates the
/// removal to the CalDAV server (spec D8; verified empirically by the
/// ticket 01 spike), so it is enough for u_crawler to remove the local file.
///
/// Carries the same `assignment_id`/`state_key` pairing as [`PlannedWrite`]
/// so the executor can, after removing the file, also drop the matching
/// `state.json` entry ([`record_deletes`]) — without that, the same
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

/// Prefix of the `state.json` namespace for a deadline `VTODO`'s projected
/// content (spec D5): `calendar:{assignment_id}`. Distinct from the content
/// flow's own `assignment:{id}` key (`syncer.rs`) — same assignment,
/// different projection, so a separate key stops the two flows from
/// clobbering each other's bookkeeping. Exposed as a constant (not just
/// baked into [`calendar_state_key`]) because the reconciliation pass in
/// [`plan`] (ticket 10) needs to recognize this namespace when scanning
/// `prev`'s keys, not just build one.
const CALENDAR_KEY_PREFIX: &str = "calendar:";

/// Prefix of the `state.json` namespace for an availability-window
/// `VEVENT`'s projected content (spec D5, ticket 08):
/// `calendar-window:{assignment_id}`. Distinct from
/// [`CALENDAR_KEY_PREFIX`] — the VTODO and VEVENT for the same assignment
/// are two different projections with two different hashes, and sharing a
/// namespace would make writing (or deleting) one component affect the
/// other regardless of its own state.
const WINDOW_KEY_PREFIX: &str = "calendar-window:";

/// The `state.json` key for a deadline `VTODO`'s projected content.
fn calendar_state_key(assignment_id: u64) -> String {
    format!("{CALENDAR_KEY_PREFIX}{assignment_id}")
}

/// The `state.json` key for an availability-window `VEVENT`'s projected
/// content.
fn window_state_key(assignment_id: u64) -> String {
    format!("{WINDOW_KEY_PREFIX}{assignment_id}")
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
fn is_submission_done(submission: &Submission) -> bool {
    submission.submitted_at.is_some() || submission.workflow_state.as_deref() == Some("graded")
}

/// Index submissions by assignment id for O(1) lookup while planning. Not a
/// `HashMap<u64, Submission>` in the public API — this is planning-internal
/// bookkeeping, not part of [`plan`]'s contract.
fn index_submissions(submissions: &[Submission]) -> HashMap<u64, &Submission> {
    submissions.iter().map(|s| (s.assignment_id, s)).collect()
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

/// Collapse every flavour of line ending Canvas can hand us (`\r\n`, lone
/// `\r`) down to a bare `\n`, so the escaper downstream sees exactly one
/// representation of "line break".
///
/// This lives here and **not** inside [`escape_text`] on purpose. `escape_text`
/// is shared with [`render_vevent`], whose bytes this ticket must not move;
/// widening it would change the `windows` collection too. The `VTODO` text
/// path is the only place that needs the normalization, so it is the only
/// place that gets it.
///
/// Why it is needed at all: a lone `\r` survives [`escape_text`] untouched and
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
/// the §3.3.11 escaping ([`escape_text`]). Order matters — normalizing after
/// escaping would leave the `\r` behind the `\n` that was already turned into
/// a `\n` escape.
fn vtodo_text(s: &str) -> String {
    escape_text(&normalize_newlines(s))
}

/// Format an instant for the *human-readable* body of the `DESCRIPTION`:
/// RFC 3339, UTC, whole seconds — `2026-09-09T14:00:00Z` (spec ID3).
///
/// Deliberately not [`ics_datetime`]'s `20260909T140000Z`: that form is for
/// property values a machine reads, this one is read by a student inside a
/// task's notes. It is UTC rather than a local zone because the project has
/// no presentation timezone and this change does not invent one (spec D9,
/// and AGENTS.md's ban on inert configuration). It also earns its keep: the
/// `DUE` that reaches Google Tasks is reduced to a bare day, so this line is
/// the only place the exact hour survives the pipeline.
fn rfc3339_utc(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Build the `DESCRIPTION` value of a deadline `VTODO` (spec ID2): three
/// logical lines, joined by the RFC 5545 §3.3.11 `\n` escape (a backslash and
/// an `n`, two octets in the file — not a real line break).
///
/// 1. the [`deadline_label`], the same text `SUMMARY` carries;
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
fn deadline_description(course: &Course, assignment: &Assignment, due: DateTime<Utc>) -> String {
    let available = match assignment.unlock_at {
        Some(unlock) => rfc3339_utc(unlock),
        None => "sin fecha de apertura".to_string(),
    };
    let mut logical_lines = vec![
        vtodo_text(&deadline_label(course, assignment)),
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
fn fold_line(line: &str) -> String {
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

/// The human-readable label for a deadline: `<course name> - <assignment
/// title>` (spec ID1).
///
/// The course name is [`Course::name`] — the human name Canvas shows — and
/// deliberately neither [`fsutil::course_dir`]'s output (sanitized and
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
fn deadline_label(course: &Course, assignment: &Assignment) -> String {
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
/// This also keeps the projection stable in the sense [`plan`]'s content-hash
/// comparison (spec D5) needs: for a given assignment/submission pair the
/// rendered content is a pure function of that data, so an unrelated field
/// changing produces a diff exactly where the data changed and nowhere else.
fn render_vtodo(
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
        format!("DUE:{}", ics_datetime(due)),
        format!("PRIORITY:{}", priority_for(assignment)),
    ];
    if done {
        lines.push("STATUS:COMPLETED".to_string());
    }
    lines.push(format!(
        "SUMMARY:{}",
        vtodo_text(&deadline_label(course, assignment))
    ));
    lines.push(format!(
        "DESCRIPTION:{}",
        deadline_description(course, assignment, due)
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
/// caller's injected instant. `submissions` is the caller's own submissions
/// for this course (spec D1, ticket 09): a deadline `VTODO` is marked
/// `STATUS:COMPLETED` when [`is_submission_done`] says so for the matching
/// assignment id, per D7 ("submitted, or graded"). No group-assignment
/// special-casing lives here — see [`is_submission_done`]'s doc comment for
/// why the bulk `self` query already covers it.
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
    _now: DateTime<Utc>,
    prev: &State,
) -> Plan {
    let course_dir = fsutil::course_dir(caldir_root, course);
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

        let uid = deadline_uid(assignment.id);
        let path = deadlines_dir.join(deadline_filename(assignment.id));
        let content = render_vtodo(&uid, course, assignment, due, done);
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

    // Reconciliation (spec D5, ticket 10): anything `prev` remembers under a
    // calendar namespace whose assignment id is no longer in `assignments`
    // has been deleted from Canvas, so its file must go too.
    let current_ids: HashSet<u64> = assignments.iter().map(|a| a.id).collect();
    let mut deletes = Vec::new();
    for key in prev.items.keys() {
        if let Some(id) = key
            .strip_prefix(CALENDAR_KEY_PREFIX)
            .and_then(|s| s.parse::<u64>().ok())
        {
            if !current_ids.contains(&id) {
                deletes.push(PlannedDelete {
                    path: deadlines_dir.join(deadline_filename(id)),
                    assignment_id: id,
                    state_key: key.clone(),
                });
            }
        } else if let Some(id) = key
            .strip_prefix(WINDOW_KEY_PREFIX)
            .and_then(|s| s.parse::<u64>().ok())
        {
            if !current_ids.contains(&id) {
                deletes.push(PlannedDelete {
                    path: windows_dir.join(window_filename(id)),
                    assignment_id: id,
                    state_key: key.clone(),
                });
            }
        }
    }

    Plan { writes, deletes }
}

/// Gate [`plan`] on both of a course's fetches having actually succeeded
/// (spec ticket 10's safety requirement): "un ramo que falla al consultarse
/// NO dispara el borrado de sus componentes". `plan` itself cannot make this
/// distinction — an empty `Vec<Assignment>` and a failed fetch look
/// identical once they are both just `&[Assignment]` — so the two have to be
/// told apart *before* `plan` is called, not inside it. `Err(())` stands in
/// for "the fetch failed"; the caller has already logged the real error by
/// the time it gets here; only the fact of failure matters for this
/// decision.
///
/// Returns `None` — no plan attempted at all, not an empty one — when either
/// fetch failed, so a network blip can never be mistaken for "Canvas says
/// this course now has zero assignments" and wipe every calendar file for
/// it. `run_calendar` is the only caller; extracted here (alongside
/// [`select_active_courses`] and [`conclude`]) so the guarantee is a
/// testable predicate rather than something only visible by reading
/// `run_calendar`'s control flow.
pub fn plan_for_course(
    caldir_root: &Path,
    course: &Course,
    assignments: Result<Vec<Assignment>, ()>,
    submissions: Result<Vec<Submission>, ()>,
    now: DateTime<Utc>,
    prev: &State,
) -> Option<Plan> {
    let assignments = assignments.ok()?;
    let submissions = submissions.ok()?;
    Some(plan(
        caldir_root,
        course,
        &assignments,
        &submissions,
        now,
        prev,
    ))
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
/// through the shared paginator), calls the pure [`plan`], and applies the
/// result with [`fsutil::atomic_write`].
///
/// Each course's submissions are fetched with one bulk call
/// ([`CanvasClient::list_submissions`], spec D1, ticket 09) rather than one
/// per assignment, and handed to [`plan`] alongside that course's
/// assignments. A submissions fetch failure is treated the same as an
/// assignments fetch failure (spec ticket 11): the course is logged and
/// skipped rather than aborting the run. The previous
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
///
/// **Deletion (spec D5, ticket 10).** [`plan`] compares `assignments`
/// against `state` and plans a delete for any component whose assignment
/// Canvas no longer mentions. [`plan_for_course`] is the gate that decides
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

        let state_path = fsutil::course_dir(&download_root, course).join("state.json");
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

/// Remove every successfully-applied delete's entry from `state` (spec D5,
/// ticket 10), the mirror of [`record_writes`]. Without this, an assignment
/// removed from Canvas would look "deleted" again on every subsequent run
/// forever: `plan` would keep finding its stale `calendar:{id}` /
/// `calendar-window:{id}` entry in `prev`, plan another (harmless but
/// pointless) delete of an already-missing file, and never let the state
/// entry go. Does not save to disk — the caller does that, and only outside
/// `--dry-run`.
fn record_deletes(state: &mut State, deletes: &[PlannedDelete]) {
    for delete in deletes {
        state.items.remove(&delete.state_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Course, Submission};

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

    /// Undo RFC 5545 §3.1 line folding, so a test can assert against one
    /// logical content line. This is the *inverse* of what the renderer
    /// does, not a copy of it: it never composes an expected value, it only
    /// makes the produced value readable. Every expected string in these
    /// tests is still a hand-written literal.
    fn unfold(content: &str) -> Vec<String> {
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
    fn logical_line(content: &str, prefix: &str) -> String {
        let matches: Vec<String> = unfold(content)
            .into_iter()
            .filter(|line| line.starts_with(prefix))
            .collect();
        assert_eq!(matches.len(), 1, "expected exactly one {prefix} line");
        matches.into_iter().next().expect("one match")
    }

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

        let got = plan(
            root,
            &long_course,
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
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

        assert_eq!(
            vtodo.content,
            concat!(
                "BEGIN:VCALENDAR\r\n",
                "VERSION:2.0\r\n",
                "PRODID:-//u_crawler//calendar-sync//EN\r\n",
                "BEGIN:VTODO\r\n",
                "UID:u_crawler-todo-555@u-crawler.local\r\n",
                "DTSTAMP:20260901T235900Z\r\n",
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
        let got = plan(
            root,
            &course(),
            &[assignment],
            &[],
            fixed_now(),
            &State::default(),
        );
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
        assert!(got.writes[0]
            .content
            .contains("SUMMARY:Intro to Testing - Essay Final"));
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

    // --- submission/completed status (ticket 09, spec D7) ---
    //
    // The three states the ticket names, asserted through the Plan's
    // rendered content (seam S3), never by calling `is_submission_done`
    // directly.

    fn submission_submitted(assignment_id: u64) -> Submission {
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

    fn submission_graded_without_submitted_at(assignment_id: u64) -> Submission {
        Submission {
            assignment_id,
            submitted_at: None,
            workflow_state: Some("graded".into()),
        }
    }

    fn submission_untouched(assignment_id: u64) -> Submission {
        Submission {
            assignment_id,
            submitted_at: None,
            workflow_state: Some("unsubmitted".into()),
        }
    }

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
        let got = plan(root, &course(), &[], &[], fixed_now(), &prev);

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
            fixed_now(),
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

        let assignments_failed: Result<Vec<Assignment>, ()> = Err(());
        let submissions_failed: Result<Vec<Submission>, ()> = Err(());

        let got = plan_for_course(
            root,
            &course(),
            assignments_failed,
            submissions_failed,
            fixed_now(),
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
        let assignments_ok: Result<Vec<Assignment>, ()> = Ok(vec![assignment_with_due_date()]);
        let submissions_failed: Result<Vec<Submission>, ()> = Err(());

        let got = plan_for_course(
            root,
            &course(),
            assignments_ok,
            submissions_failed,
            fixed_now(),
            &State::default(),
        );

        assert!(got.is_none());
    }

    #[test]
    fn both_fetches_succeeding_produces_a_plan() {
        let root = Path::new("/caldir");
        let assignments_ok: Result<Vec<Assignment>, ()> = Ok(vec![assignment_with_due_date()]);
        let submissions_ok: Result<Vec<Submission>, ()> = Ok(vec![]);

        let got = plan_for_course(
            root,
            &course(),
            assignments_ok,
            submissions_ok,
            fixed_now(),
            &State::default(),
        )
        .expect("both fetches succeeded");

        assert_eq!(got.writes.len(), 1);
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

    #[test]
    fn an_unrelated_state_key_with_the_calendar_prefix_as_a_substring_is_not_touched() {
        // `state.json` is shared with `syncer.rs`, which uses its own
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

        let got = plan(root, &course(), &[], &[], fixed_now(), &prev);

        assert!(got.deletes.is_empty());
    }
}
