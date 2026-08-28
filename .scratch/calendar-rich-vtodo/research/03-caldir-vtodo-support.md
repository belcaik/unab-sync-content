# caldir `vtodo-support`: how a VTODO is parsed, modelled, serialized and projected to Google Tasks

**Source of truth:** read-only clone at
`/tmp/claude-1000/-home-belcaik-Dev-unab-sync-content/0c205e29-1023-4b26-a55a-e7a544127f53/scratchpad/deps/caldir`,
branch `vtodo-support`, HEAD `02ee000 feat(google): G1-01 request the calendar.calendars scope`.
All line numbers below are from that checkout. No file in the clone was modified.

Every claim below is grounded in code or in caldir's own committed docs. Where I could not verify
something from source (the `icalendar` 0.17.10 crate is not vendored and is not in the local cargo
registry), it is labelled **UNVERIFIED**.

---

## Verdict

### Per-property survival table

Local round trip = `Todo::try_from(&icalendar::Todo)` → `Todo::to_ics_string()` → reparse.
"Sent to Google" = present in the `tasks.insert` / `tasks.patch` JSON body.

| Property | Parsed into `Todo`? | Re-serialized to ICS? | Sent to Google Tasks? | As what / notes |
|---|---|---|---|---|
| `UID` | yes → `uid` | yes | **no** | Google id lives in `X-GOOGLE-TASK-ID`; local UID is preserved by the merge |
| `DTSTAMP` | no (never modelled) | yes, **re-stamped fresh every write** | no | `to_icalendar.rs:116-118` |
| `SUMMARY` | yes → `summary` (empty → `None`) | yes | **yes** | `title` (verbatim) |
| `DESCRIPTION` | yes → `description` (empty → `None`) | yes | **yes** | `notes` (verbatim). Multi-line survives as escaped `\n` + folding |
| `DUE` (UTC datetime) | yes → `EventTime::DateTimeUtc` | yes, byte-identical UTC form | **yes, LOSSY** | `due: "YYYY-MM-DDT00:00:00.000Z"` — **time-of-day destroyed**, date computed in the *local* zone |
| `DTSTART` | yes → `start` | yes, byte-identical | **NO — never sent** | Preserved only locally |
| `URL` | yes → `url` | yes | **NO — never sent** | Preserved only locally; explicitly never appended to `notes` |
| `PRIORITY` | yes → `priority: Option<i32>`, raw/unclamped | yes, verbatim | **NO — never sent** | Preserved only locally |
| `STATUS` | yes → 4-valued `TodoStatus`; unknown value **silently dropped** to `NEEDS-ACTION` | yes (omitted at the RFC default) | **partially** | `COMPLETED`/`NEEDS-ACTION` faithful; `IN-PROCESS`/`CANCELLED` sent as `needsAction` + stderr warning, local value restored |
| `COMPLETED` | yes → `EventTime` (floating/zoned survive) | yes | **yes, lossy** | RFC-3339 instant truncated to milliseconds |
| `PERCENT-COMPLETE` | yes → `Option<i32>`, raw | yes | **NO** | Preserved only locally |
| `LOCATION` | yes → `location` | yes | **NO** | Preserved only locally |
| `ORGANIZER` / `ATTENDEE` | yes | yes | **NO** | Preserved only locally |
| `ATTACH` (URI) | yes; inline binary dropped | yes | **NO** | Preserved only locally |
| `VALARM` | yes → `reminders` | yes (hand-spliced) | **NO** | Preserved only locally |
| `RECURRENCE-ID` | yes | yes | n/a | Only on a recurring instance → transitively rejected |
| `RRULE`/`EXDATE`/`RDATE` | yes → `recurrence` | yes | **REJECTED** | Push refused before any network call with a typed `SemanticLimitation` error |
| `CREATED` (UTC only) | yes | yes | **NO** | Non-UTC `CREATED` is dropped |
| `LAST-MODIFIED` | yes | yes | **NO** | |
| `SEQUENCE` | yes (lenient: unparseable → 0) | yes (omitted at 0) | **NO** | |
| `X-*` | yes, with parameters | yes, verbatim | **NO** | Except `X-GOOGLE-TASK-ID`, owned/rewritten by the provider |
| `CATEGORIES`, `GEO`, `RELATED-TO`, `RESOURCES`, `COMMENT`, `CONTACT`, `CLASS`, `DURATION`, any other IANA/vendor property | **NO** | **NO — dropped on caldir's first write** | no | There is **no** generic passthrough. Only `X-` survives. |
| `DTEND` | **NO** (forbidden on VTODO) | never emitted | no | Actively guarded by a byte-level test |

### The three headline facts

1. **`DTSTART` never reaches Google Tasks.** The insert/patch bodies have no field for it
   (`to_google.rs:17-45`), the mapper never reads `Todo.start` (`to_google.rs:82-104`), and this is
   a *deliberate, tested, documented* classification, not an oversight.
2. **A pushed `DUE` datetime is downgraded to a date — and the downgrade is written back into the
   local `.ics` file.** `merge_canonical_task_response` overwrites `merged.due` with Google's
   date-only echo (`create_event.rs:100`), and core then rewrites the local file with exactly that
   (`connection.rs:267-271`). `DUE:20260902T035959Z` becomes `DUE;VALUE=DATE:20260901` on disk, and
   the file is *renamed* because the filename slug is derived from `DUE`.
3. **Only `X-` properties pass through.** Anything caldir does not model is destroyed on the first
   caldir write — locally and on the server.

---

## 1. The `Todo` model

`caldir-core/src/todo.rs:28-79`:

```rust
#[derive(Debug, Clone, Eq, educe::Educe)]
#[educe(PartialEq)]
pub struct Todo {
    pub uid: EventUid,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub location: Option<String>,
    /// `DTSTART`. Independent of `due` — a task may have either, both or
    /// neither, and neither is ever synthesized from the other.
    pub start: Option<EventTime>,
    /// `DUE`. The task's anchor.
    pub due: Option<EventTime>,
    pub status: TodoStatus,
    pub completed: Option<EventTime>,
    pub percent_complete: Option<i32>,
    pub priority: Option<i32>,
    pub url: Option<String>,
    pub recurrence: Option<Recurrence>,
    pub recurrence_id: Option<RecurrenceId>,
    pub organizer: Option<Organizer>,
    pub attendees: Vec<Attendee>,
    pub reminders: Vec<Reminder>,
    #[educe(PartialEq(method(attachments_eq)))]
    pub attachments: Vec<Attachment>,
    #[educe(PartialEq(method(x_properties_eq)))]
    pub x_properties: Vec<XProperty>,
    #[educe(PartialEq(ignore))]
    pub created: Option<DateTime<Utc>>,
    #[educe(PartialEq(ignore))]
    pub last_modified: Option<DateTime<Utc>>,
    #[educe(PartialEq(ignore))]
    pub sequence: i32,
}
```

**Yes — there is both a `start` field and a `due` field**, both `Option<EventTime>`, both first
class, neither synthesized from the other (`todo.rs:35-39`). There is deliberately **no** `end` or
`duration` field (`todo.rs:22-27`): `DTEND` is RFC-invalid on a VTODO, and the field simply not
existing is what makes emitting one impossible.

### date vs date-time: `EventTime`

`caldir-core/src/event/time.rs:5-14`:

```rust
pub enum EventTime {
    Date(NaiveDate),
    DateTimeUtc(DateTime<Utc>),
    DateTimeFloating(NaiveDateTime),
    DateTimeZoned { datetime: NaiveDateTime, tzid: String },
}
```

A single enum carrying all four shapes. "Date only" is `EventTime::Date`; a UTC instant is
`DateTimeUtc`; a floating (zoneless) value and a `TZID`-carrying value are distinct variants, so
neither collapses into the other. Conversion from the crate's `DatePerhapsTime` is at `time.rs:111-145`;
an inbound Windows TZID is normalized to IANA there (`Tzid::Iana`), and a fixed-offset TZID is
folded into `DateTimeUtc`.

`Todo::occurs_in_range` (`todo.rs:127-143`) implements RFC 4791 §9.9's VTODO time-range table over
`(start, due)`; an undated task (`None, None`) is **unconditionally in-window** (`todo.rs:141`).

Equality (`Todo`'s `PartialEq`) *is* the sync engine's "content changed?" predicate. `created`,
`last_modified` and `sequence` are excluded (`todo.rs:68-78`); `due` and `start` are included, and
`every_content_field_participates_in_equality` (`todo.rs:346`) destructures `Todo` exhaustively
with no `..` so a new field cannot silently fall out of `==`.

---

## 2. The iCalendar codec

### 2.1 Parser — `caldir-core/src/todo/from_icalendar.rs`

`impl TryFrom<&icalendar::Todo> for Todo` (`:9-118`). Recognized properties:

| line | property → field |
|---|---|
| `:13` | `UID` → `uid` (the **only** hard-required property; missing → `EventError::MissingUid`) |
| `:24` | `DTSTART` → `start` |
| `:26` | `DUE` → `due` |
| `:31-32` | `COMPLETED` → `completed` (raw, so floating/zoned survive) |
| `:39-42` | `STATUS` → `TodoStatus` (raw) |
| `:49` | `PRIORITY` → `priority` (raw `i32`) |
| `:53-58` | `X-*` → `x_properties` |
| `:60` | `RRULE`/`EXDATE`/`RDATE` → `recurrence` |
| `:62-65` | `RECURRENCE-ID` → `recurrence_id` |
| `:67` | `ORGANIZER` → `organizer` |
| `:69-73` | `ATTENDEE` (multi) → `attendees` |
| `:75` | `VALARM` → `reminders` |
| `:79-83` | `ATTACH` (multi; URI kept, inline binary dropped) → `attachments` |
| `:87` | `SUMMARY` → `summary` (empty string → `None`) |
| `:88` | `DESCRIPTION` → `description` (empty string → `None`) |
| `:91` | `LOCATION` → `location` (read raw) |
| `:99` | `PERCENT-COMPLETE` → `percent_complete` |
| `:101` | `URL` → `url` |
| `:109` | `CREATED` → `created` (UTC-only; non-UTC dropped, `:129-137`) |
| `:110` | `LAST-MODIFIED` → `last_modified` |
| `:113-116` | `SEQUENCE` → `sequence` (unparseable → `0`, deliberately lenient) |

**Yes — parsing a VTODO with `DTSTART` populates `Todo.start`** (`from_icalendar.rs:24-25`):

```rust
let start =
    present_or_unparseable(value, "DTSTART", value.get_start().map(EventTime::from))?;
let due = present_or_unparseable(value, "DUE", value.get_due().map(EventTime::from))?;
```

`present_or_unparseable` (`:155-167`) is what distinguishes *absent* (legal → `None`) from *present
but unreadable* (`EventError::UnparseableProperty`):

```rust
match (parsed, value.properties().get(name)) {
    (None, Some(property)) => Err(EventError::UnparseableProperty {
        name: name.to_string(),
        value: property.value().to_string(),
    }),
    (parsed, _) => Ok(parsed),
}
```

So `DTSTART:garbage` is a **hard error**, not a silent drop — pinned by
`an_unparseable_property_is_an_error_rather_than_a_silent_drop` (`:445-473`) for `DUE`, `DTSTART`,
`COMPLETED`, `PRIORITY` and `PERCENT-COMPLETE`.

### 2.2 Serializer — `caldir-core/src/todo/to_icalendar.rs`

`impl From<&Todo> for icalendar::Todo` (`:13-122`). **Yes — a `Todo` with a `start` re-emits
`DTSTART`** (`:20-26`):

```rust
// Both anchors are independent and neither is ever synthesized from the
// other. `due` must NEVER be routed through `ends()`.
if let Some(start) = &value.start {
    todo.append_property(DatePerhapsTime::from(start).to_property("DTSTART"));
}

if let Some(due) = &value.due {
    todo.append_property(DatePerhapsTime::from(due).to_property("DUE"));
}
```

Emitted: `UID`, `DTSTART`, `DUE`, `STATUS` (omitted at the `NEEDS-ACTION` default, `:32-34`),
`COMPLETED`, `PERCENT-COMPLETE`, `PRIORITY` (raw, never through `Component::priority()` which
clamps to 10, `:46-51`), `RRULE`/`EXDATE`/`RDATE`, `RECURRENCE-ID`, `SUMMARY`, `DESCRIPTION`,
`LOCATION`, `CREATED`, `LAST-MODIFIED`, `SEQUENCE` (omitted at 0), `ORGANIZER`, `ATTENDEE`, `URL`
(`:104-106`), `ATTACH`, `X-*`. `VALARM`s are spliced into the string by
`Todo::splice_valarms_into_vtodo` (`todo.rs:165-173`) rather than via the crate, because the crate
mints a fresh UUID+DTSTAMP into every serialized VALARM and would make two generations compare
unequal forever.

`DTSTAMP` is never modelled and is stamped fresh at write time (`to_icalendar.rs:116-118`).
`DTEND`/`DURATION` are never emitted, guarded by an emitted-bytes test
(`serializing_a_task_never_emits_dtend`, `:273-315`) that first proves the leak is real on the
pinned crate and then asserts caldir does not take it.

**DATE-TIME UTC form is preserved.** `EventTime::DateTimeUtc` → `DatePerhapsTime::DateTime(Utc)` →
`YYYYMMDDTHHMMSSZ`. Asserted literally (`to_icalendar.rs:298-304`):

```rust
assert!(ics.contains("DTSTART:20260810T090000Z\r\n"), "DTSTART must be emitted: {ics}");
assert!(ics.contains("DUE:20260815T235900Z\r\n"),     "DUE must be emitted: {ics}");
```

A zoned value keeps its TZID: `DUE;TZID=Europe/Stockholm:20260815T235900\r\n` (`:248-264`). A
floating `COMPLETED` is emitted without `Z` (`:230-243`). A date-only value emits `;VALUE=DATE:`
(seen at `caldir-cli/src/output/agenda.rs:409`, `DUE;VALUE=DATE:20260814`).

### 2.3 Passthrough / unknown-property preservation — **`X-` only**

`from_icalendar.rs:52-58`:

```rust
// The same filter events use. Only `X-` properties are
// captured; nothing else is ever put into a passthrough bag.
let x_properties = value
    .properties()
    .iter()
    .filter(|(name, _)| name.starts_with("X-"))
    .map(|(_, prop)| XProperty::from(prop))
    .collect();
```

There is **no** generic unknown-property bag. The documented drop list is pinned as a literal
(`todo.rs:705-721`):

```rust
const R15_DROPPED_PROPERTIES: &[&str] = &[
    "CATEGORIES", "GEO", "RELATED-TO", "RESOURCES", "COMMENT", "CONTACT",
];
const OTHER_UNMODELLED_PROPERTIES: &[&str] = &["CLASS", "DURATION", "REQUEST-STATUS"];
```

`dropped_property_names_are_exactly_the_documented_list` (`todo.rs:842-901`) asserts set equality
in both directions: exactly those names are lost, nothing is invented, and every modelled name
survives.

The user guarantee, `docs/vtodo/spec.md:206-210`:

> **R15.** The user guarantee, for the spec and docs, MUST be: *caldir round-trips a task's
> modelled properties plus every `X-` property verbatim, parameters included; any other property —
> `CATEGORIES`, `GEO`, `RELATED-TO`, `RESOURCES`, `COMMENT`, `CONTACT`, and any other IANA or
> vendor property caldir does not model — is dropped the first time caldir writes that item, on
> disk and on the server alike, exactly as it already is for events.*

### 2.4 TEXT escaping and line folding

caldir does **no** escaping or folding of its own in either direction; it delegates entirely to
`icalendar = 0.17.10` (`Cargo.lock`). Evidence that the crate's fold/unfold + escape/unescape pair
is byte-symmetric comes from the **event** codec, whose fixture carries both a folded line and a
literal `\n` escape (`caldir-core/src/event.rs:419-421`):

```
DESCRIPTION:https://docs.example.com/document/d/abc123def456ghijklmnopqrstu
 v/edit?usp=sharing\n
```

and asserts (`event.rs:458`):

```rust
assert_eq!(strip_dtstamp(&original_ics), strip_dtstamp(&serialized_ics));
```

i.e. **byte-identical** apart from the re-stamped `DTSTAMP`. The domain-level pair is
`converts_description` on both sides, which round-trips `"Multi-line\nnotes"`
(`event/from_icalendar.rs:196-203`, `event/to_icalendar.rs:144-152`).

The VTODO path calls the same crate methods (`todo.summary(...)`, `todo.description(...)` at
`to_icalendar.rs:63-69`) through the same `Calendar::to_string()`, so the behaviour is the same.

**Gaps worth flagging:**

- There is **no VTODO-specific test** for a folded or multi-line `DESCRIPTION`. In fact the VTODO
  loss-set test asserts the *opposite* — that no line in its fixture is folded
  (`todo.rs:814-817`: `"folded line would defeat name extraction: {line}"`). The multi-line
  guarantee for tasks is inherited from the event path, not independently pinned. **UNVERIFIED for
  VTODO specifically.**
- The pinned crate escapes commas in property values on write: caldir's own spec records
  `CATEGORIES:WORK,URGENT` re-emitting as `WORK\,URGENT` (`docs/vtodo/spec.md:197`, "verified,
  pinned `=0.17.10`"). Whether that escaping also applies to the URI-typed `URL` property (where
  RFC 5545 forbids escaping) is **not tested anywhere in caldir** and I could not read the crate
  source. A `URL` containing a literal `,` or `;` is an untested edge. **UNVERIFIED.**
- Non-ASCII UTF-8 in `SUMMARY` is explicitly pinned as byte-stable
  (`from_icalendar.rs:580-593`, `"SUMMARY:Sube aquí tu Certificado: Mentalidad digital"`).
  Unescaped colons inside TEXT values survive, as that same assertion shows.

---

## 3. `caldir-provider-google/src/google_tasks/`

Five files plus `mod.rs`. Everything Tasks-specific is private to this module; only
`caldir_core::CalendarItem` / `Todo` crosses out (`mod.rs:1-15`).

### 3.1 `to_google.rs` — what actually goes on the wire

The two wire bodies (`:17-45`):

```rust
pub struct GoogleTaskInsert {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub notes: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<String>,
}

pub struct GoogleTaskPatch {
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
    pub due: Option<String>,
    pub completed: Option<String>,
}
```

Note the asymmetry, documented at `:34-37`: PATCH treats an omitted field as "leave alone" and
needs an explicit `null` to clear, so `notes`/`due`/`completed` **never skip serialization** in the
patch body. A local task with `description = None` therefore sends `"notes": null` and **clears
whatever notes Google held**.

The mapping (`:82-104`):

```rust
impl ToGoogle for Todo {
    fn to_google<Tz: TimeZone>(&self, zone: &Tz) -> GoogleTaskInsert {
        let status = match self.status {
            TodoStatus::Completed => "completed",
            _ => "needsAction",
        }.to_string();

        GoogleTaskInsert {
            id: todo_x_property(self, PROVIDER_TASK_ID_PROPERTY).map(str::to_string),
            title: self.summary.clone().unwrap_or_default(),
            notes: self.description.clone().unwrap_or_default(),
            status,
            due: self.due.as_ref().map(|due| due_to_google(due, zone)),
            completed: self.completed.as_ref().map(|c| completed_to_google(c, zone)),
        }
    }
}
```

Field-by-field answers to the brief:

- **`title`** ← `Todo.summary` (`unwrap_or_default()`, so a summary-less task sends `""`).
- **`notes`** ← `Todo.description`, and **nothing else**. Not the URL, not attachments, not
  priority — each of those has an explicit negative test.
- **`due`** ← `Todo.due` only.
- **`completed`** ← `Todo.completed`.
- **`status`** ← `Todo.status`, collapsed to two values.
- **`id`** ← the `X-GOOGLE-TASK-ID` x-property, if any.
- **`Todo.start` (`DTSTART`) is read nowhere.** There is no field for it in either body.
- **`URL` is sent nowhere.**

The conversions (`:106-121`):

```rust
/// `DUE` -> `due`: the calendar date under `zone` (`super::due::
/// google_due_date`), sent as midnight UTC — the shape Google's `due`
/// field expects (date precision only, §13 clause 1).
fn due_to_google<Tz: TimeZone>(due: &EventTime, zone: &Tz) -> String {
    let date = super::due::google_due_date(due, zone);
    format!("{}T00:00:00.000Z", date.format("%Y-%m-%d"))
}

/// `COMPLETED` -> `completed`, an RFC 3339 instant (unlike `due`, Google
/// keeps time precision here).
fn completed_to_google<Tz: TimeZone>(completed: &EventTime, zone: &Tz) -> String {
    completed.to_local_tz(zone).with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
```

### 3.2 `due.rs` — the DUE conversion

The whole of it (`:22-32`):

```rust
/// A datetime `DUE`'s calendar date under `tz`, resolved through the zone
/// database. ... **Do not** take a shortcut through `EventTime::to_utc` —
/// it is `to_local_tz(&chrono::Local)` and answers a different question
/// (an instant, not a calendar date in `tz`).
pub fn google_due_date<Tz: TimeZone>(due: &EventTime, tz: &Tz) -> NaiveDate {
    due.to_local_tz(tz).date_naive()
}

/// The inverse: a date-only `DUE`, never a midnight timestamp (§13 clause
/// 2). Google's `due` field carries a timestamp, but the API discards its
/// time-of-day — reading that discarded midnight back as a `DateTimeUtc`
/// is how the churn starts.
pub fn date_to_due(date: NaiveDate) -> EventTime {
    EventTime::Date(date)
}
```

So: **date-only normalization, computed in a caller-supplied zone, then padded back to
`T00:00:00.000Z`.** The time of day is destroyed. This is not "truncation to midnight UTC" — the
calendar date is taken *in the local zone first*, which is a different answer. The worked example
in the tests (`due.rs:51-63`): a production `DUE:20260902T035959Z` is 23:59:59 on **2026-09-01** in
`America/Santiago`, so it is sent as `2026-09-01T00:00:00.000Z`. Using `to_utc()` would send
`2026-09-02` and move the entire u_crawler deadline set one day late.

The zone actually used in production is **`chrono::Local`** — the machine's timezone:

- `create_event.rs:72`: `let mut body = todo.to_google(&chrono::Local);`
- `update_event.rs:80`: `let body = todo.to_google_patch(&chrono::Local);`

The DST offset comes from the zone database, not a constant (`due.rs:76-87`).

### 3.3 `from_google.rs` — reading back, and what is lost

`task_to_todo` (`:29-67`) builds a `Todo` from exactly six wire fields and **nothing else**:

```rust
let mut todo = Todo::new(task.title.clone());
todo.uid = derive_task_uid(&task.id);
todo.summary = if task.title.is_empty() { None } else { Some(task.title) };
todo.description = if task.notes.is_empty() { None } else { Some(task.notes) };
todo.due = due;
todo.status = status;
todo.completed = completed;
todo.x_properties = vec![XProperty::new(PROVIDER_TASK_ID_PROPERTY, task.id)];
```

Everything else on `Todo` is left at `Todo::new`'s defaults — so a task read *fresh* from Google
has **no `start`, no `url`, no `priority`, no `location`, no attendees, no alarms, no attachments,
no `created`/`last_modified`**.

`due` comes back date-only (`:86-89`):

```rust
fn google_due_to_date(due: &str) -> Option<EventTime> {
    let dt = chrono::DateTime::parse_from_rfc3339(due).ok()?;
    Some(super::due::date_to_due(dt.date_naive()))
}
```

The calendar date is taken from the timestamp's **own** offset, never from `chrono::Local` (doc
comment `:82-85`) — asymmetric with the write direction on purpose, because Google always sends `Z`.

`deleted: true` → `None` (item disappears from the listing). `hidden: true` is **not** a delete
(`:25-28`) — Google hides completed tasks in its own UI.

UID minting (`:21-23`): `format!("{google_task_id}@google-tasks.com")`. A task first seen remotely
gets that UID; a locally-created task keeps its own UID through the merge (below).

**Does a round trip through Google preserve `DTSTART`? Only because the merge carries it forward
from the local item — never because Google returned it.** `from_google` alone loses it completely.

### 3.4 `policy.rs` — the authority / merge policy

Two runtime functions plus a 20-test classification table.

`reject_recurring` (`:34-40`) — the one **rejected** row. Called before any network call in both
`create_event.rs:30-35` and `update_event.rs:33`, wrapped as a typed
`ErrorCode::SemanticLimitation`, so the local file is never touched:

```rust
pub(crate) fn reject_recurring(todo: &Todo) -> Result<(), RecurringTaskRejected> {
    if todo.recurrence.is_some() { Err(RecurringTaskRejected) } else { Ok(()) }
}
```

`semantic_change_warning` (`:47-58`) — `IN-PROCESS`/`CANCELLED` produce a stderr warning saying the
change will not reach Google.

The authoritative classification is the exhaustive destructure at `:473-497`:

```rust
let Todo {
    uid: _,              // identity, out of this table
    summary: _,          // faithful
    description: _,      // faithful
    location: _,         // preserved only locally (not sent; no Tasks location concept)
    start: _,            // preserved only locally (DTSTART)
    due: _,              // lossy (date-only) / faithful for date-only input
    status: _,           // faithful (NEEDS-ACTION/COMPLETED) / lossy (IN-PROCESS/CANCELLED)
    completed: _,        // lossy (millisecond precision)
    percent_complete: _, // preserved only locally
    priority: _,         // preserved only locally
    url: _,              // preserved only locally
    recurrence: _,       // rejected
    recurrence_id: _,    // out of this table
    organizer: _,        // preserved only locally
    attendees: _,        // preserved only locally
    reminders: _,        // preserved only locally (VALARM)
    attachments: _,      // preserved only locally (ATTACH)
    x_properties: _,     // preserved only locally (except X-GOOGLE-TASK-ID)
    created: _, last_modified: _, sequence: _,
} = todo;
```

No `..`, so adding a field to `Todo` breaks compilation here rather than silently classifying it as
"dropped".

**Which side wins for which field.** The merge seam is
`merge_canonical_task_response` in `caldir-provider-google/src/commands/create_event.rs:83-117`,
shared by create and update:

```rust
let local_status = local.status;
let mut merged = local;
merged.summary = canonical.summary;
merged.description = canonical.description;
merged.status = canonical.status;
merged.completed = canonical.completed;
merged.due = canonical.due;
merged.x_properties.retain(|p| p.name != PROVIDER_TASK_ID_PROPERTY);
merged.x_properties.extend(canonical.x_properties);

if matches!(local_status, TodoStatus::InProcess | TodoStatus::Cancelled) {
    merged.status = local_status;
}
```

- **Google wins** on: `summary`, `description`, `status`, `completed`, `due`, `X-GOOGLE-TASK-ID`.
- **Local wins** (carried forward untouched) on: `uid`, `location`, **`start`**, `percent_complete`,
  `priority`, **`url`**, `recurrence_id`, `organizer`, `attendees`, `reminders`, `attachments`,
  every other `X-`, `created`, `last_modified`, `sequence`.
- **Local wins with a carve-out** on `status` when it was `IN-PROCESS`/`CANCELLED` — otherwise
  Google's echoed `needsAction` would collapse the local value on every push.

Why the merge is load-bearing (`docs/vtodo/provider-sync/providers/google-tasks.md:153-158`):

> `push_outgoing_changes` (`caldir-core/src/connection.rs`) calls `cal_event.update(returned_item)` —
> core overwrites the local `.ics` with exactly what the provider returns from its create/update
> response. A field is preserved only if the provider's response-merge copies it from the request
> onto the parsed response, not by virtue of "the wire format doesn't carry it."

Verified in core: `connection.rs:267-271`.

### 3.5 `client.rs` — the actual REST calls

| function | verb + endpoint | body |
|---|---|---|
| `list_task_lists` (`:26-46`) | `GET https://tasks.googleapis.com/tasks/v1/users/@me/lists` | — |
| `list_tasks` (`:98-123`) | `GET .../lists/{id}/tasks?showDeleted=true&showHidden=true&showCompleted=true` | — |
| `insert_task` (`:128-151`) | `POST .../lists/{id}/tasks` | `GoogleTaskInsert` as JSON |
| `patch_task` (`:172-191`) | `PATCH .../lists/{id}/tasks/{task}` | `GoogleTaskPatch` as JSON |
| `delete_task` (`:158-167`) | `DELETE .../lists/{id}/tasks/{task}` | — |

Auth is a bearer token from the existing session store. The response struct `Task` (`:56-76`) has
exactly `id`, `title`, `notes`, `status`, `due`, `completed`, `deleted`, `hidden` — `updated` is
read from the wire but not mapped, so `Todo.last_modified` is never populated from Google.

`insert`/`patch`/`delete` all hit `tasks.googleapis.com`, never the Calendar API — guarded by
`a_task_delete_never_reaches_the_calendar_events_endpoint` (`delete_event.rs:153`) and
`a_task_create_targets_the_tasks_endpoint_not_the_calendar_one` (`create_event.rs:367`).

`task_insert_request` clears `body.id = None` (`create_event.rs:73`) — a client-set resource id gets
"Invalid resource id value" back from Google.

### 3.6 Routing

A Google **Calendar** remote advertises `tasks: {read: false, write: false}`
(`commands/capabilities.rs:18-32`), so core holds a VTODO back rather than pushing it there. Only a
`google_resource_kind = "tasklist"` remote accepts tasks, and only with the
`https://www.googleapis.com/auth/tasks` scope (`session.require_tasks_scope()`).

---

## 4. Tests

No `.ics` fixture **files** exist anywhere in the repo — every fixture is a Rust `&str` constant.
`find . -name '*.ics'` returns nothing.

### VTODO codec (caldir-core)

| file | what it asserts |
|---|---|
| `caldir-core/src/todo.rs:197-1017` | equality table (`equality_mirrors_event_equality`, `every_content_field_participates_in_equality`); `valarm_is_byte_stable_across_three_generations`; `serializing_the_same_task_twice_differs_only_in_dtstamp`; **`a_fully_populated_task_survives_serialize_and_reparse`** (`:645`, the round trip the sync engine depends on); `new_synthesizes_neither_a_start_nor_a_due`; `dropped_property_names_are_exactly_the_documented_list` (`:842`, the loss-set pin); `two_related_to_lines_collapse_to_one`; the two production-fixture pins |
| `caldir-core/src/todo/from_icalendar.rs:184-594` | `errors_when_the_task_has_no_uid`; **`parses_the_rest_of_the_mvp_property_set`** (`:218`, a fully-populated VTODO **with `DTSTART;TZID=W. Europe Standard Time`** asserted to normalize to `Europe/Berlin`); `x_properties_are_captured_with_parameters_and_nothing_else_is`; `a_task_needs_no_dtstart_and_never_captures_a_dtend`; `a_due_with_a_windows_tzid_is_normalized_to_iana`; `an_unparseable_property_is_an_error_rather_than_a_silent_drop`; `an_out_of_range_integer_round_trips_verbatim_rather_than_vanishing`; `zoned_due_and_floating_completed_round_trip`; `the_open_production_fixture_parses_with_every_property_intact`; `a_production_summary_survives_its_non_ascii_text` |
| `caldir-core/src/todo/to_icalendar.rs:130-315` | `priority_is_emitted_verbatim_rather_than_clamped`; `rfc_default_status_and_sequence_are_omitted`; `unset_properties_emit_no_line`; `a_floating_completed_is_emitted_without_a_z_suffix`; `a_zoned_due_keeps_its_tzid`; **`serializing_a_task_never_emits_dtend`** (`:273`, asserts the exact `DTSTART:...Z` / `DUE:...Z` bytes) |
| `caldir-core/src/todo/status.rs:56` | the four statuses round-trip |
| `caldir-core/src/item.rs:363-648` | one-ordered-pass parsing, VTIMEZONE arity, VEVENT+VTODO identity separation |
| `caldir-core/src/connection.rs:~2250-2340` | `syncing_twice_against_a_fake_remote_yields_an_empty_second_diff` — the idempotence gate, driven from **raw ICS** via `reply_raw_data` |

### Google Tasks projection (caldir-provider-google)

| file | tests |
|---|---|
| `google_tasks/to_google.rs:123-163` | `the_faithful_set_reaches_the_google_task`; `a_never_synced_task_is_sent_without_a_client_chosen_id` (checks the JSON, not just the Rust type) |
| `google_tasks/due.rs:34-186` | 7 tests: local-calendar-date preservation on two production deadlines, DST offset from the zone DB, no "*5959Z" string rule, `TZID` beats the system zone, fixpoint under 4 zones × 5 dues, `an_undated_task_stays_undated` |
| `google_tasks/from_google.rs:91-233` | task→Todo never→Event; title/notes→summary/description; completed status+time; **`a_google_due_becomes_a_date_only_due`** and `..._never_a_midnight_timestamp`; task id is an x-property never the UID; deleted dropped / hidden kept |
| `google_tasks/policy.rs:60-499` | the 20-row classification, incl. **`dtstart_survives_the_push_and_is_never_folded_into_due`** (`:285`), `url_survives_the_push_and_is_never_appended_to_notes` (`:266`), `priority_survives_the_push_and_never_enters_title_or_notes` (`:246`), `a_timestamped_due_is_classified_lossy_not_faithful` (`:167`), `the_ucrawler_fixture_survives_a_push_with_priority_and_url_intact` (`:443`), `every_todo_field_appears_in_the_classification` (`:473`) |
| `google_tasks/mod.rs:34-280` | id-vs-UID separation; id appears exactly once after two cycles; patch/delete resolve to the same URL; **`the_whole_fixture_set_canonicalizes_to_a_fixpoint`** (`:245`) over four VTODO fixtures |
| `commands/create_event.rs:367-414` | task create targets the Tasks endpoint; Google's generated id reaches core |
| `commands/update_event.rs:177-297` | patch targets the stored id; two tasks with the same summary resolve differently; no stored id is a named refusal; `the_task_patch_body_carries_no_event_fields`; canonical response merge |
| `commands/delete_event.rs:116-158` | delete targets the stored id; 404 = success; a real failure is not read as already-deleted |
| `commands/capabilities.rs` | tasklist remote advertises task read+write; a Calendar remote advertises neither; unresolvable → events-only |
| `live/google_tasks.rs` (619 lines) | 5 **runtime-gated** live tests against a real account: `create_sync_sync_yields_exactly_one_remote_task`, `create_sync_sync_reports_no_changes_for_a_timestamped_due`, `create_sync_sync_reports_no_changes`, `update_sync_sync_reports_no_changes`, `delete_sync_reports_absent` |

### Fixture `.ics` constants containing a VTODO with `DTSTART`

Only two, both Rust constants, both in `caldir-core`. The parser-side one
(`caldir-core/src/todo/from_icalendar.rs:219-247`):

```
BEGIN:VCALENDAR
VERSION:2.0
BEGIN:VTODO
UID:full@caldir
DTSTAMP:20260801T090000Z
DTSTART;TZID=W. Europe Standard Time:20260810T090000
DUE:20260815T235900Z
SUMMARY:Everything
DESCRIPTION:All the rows
LOCATION:Room 3
URL:https://example.edu/a/1
PERCENT-COMPLETE:25
CREATED:20260701T080000Z
LAST-MODIFIED:20260802T101500Z
SEQUENCE:4
RECURRENCE-ID:20260815T235900Z
ORGANIZER;CN=Boss:mailto:boss@example.com
ATTENDEE;CN=Ann:mailto:ann@example.com
ATTACH:https://example.com/a.pdf
RRULE:FREQ=WEEKLY
EXDATE:20260822T235900Z
RDATE:20260829T235900Z
BEGIN:VALARM
ACTION:DISPLAY
DESCRIPTION:Reminder
TRIGGER:-PT15M
END:VALARM
END:VTODO
END:VCALENDAR
```

The other is `FULL_VTODO_ICS` (`caldir-core/src/todo.rs:511-544`), same shape with
`DTSTART;TZID=Europe/Stockholm:20260810T090000`, `STATUS:IN-PROCESS`, `PRIORITY:42` and
`X-GOOGLE-ID;X-SOURCE=api:abc123`.

**Note: no Google-Tasks-side fixture carries `DTSTART`.** The four fixtures copied into
`google_tasks/mod.rs:148-203` are all `DTSTART`-free. `dtstart_survives_the_push_and_is_never_folded_into_due`
(`policy.rs:285`) builds its `Todo` in Rust rather than from ICS.

---

## 5. Documentation

### The definitive Google Tasks mapping doc

`docs/vtodo/provider-sync/providers/google-tasks.md:146-151`:

> **Grounded in production, not hypotheticals.** The `P1-05` fixture archive (57 VTODOs from a real
> `u_crawler` deployment) carries exactly `UID`, `DTSTAMP`, `DUE`, `PRIORITY`, `SUMMARY`, `URL` — and
> `STATUS:COMPLETED` alone (no `COMPLETED` timestamp, no `PERCENT-COMPLETE`) on 32 of the 57. No
> fixture carries `DESCRIPTION`, `DTSTART`, `RRULE`, `ATTENDEE`, `ORGANIZER`, `ATTACH`, `VALARM`, or any
> `X-` property. `PRIORITY:1` and `URL` are on **all 57** — the two rows that matter most in
> production are two of the rows Google Tasks cannot carry at all.

`:182` — the `DTSTART` row of the table, verbatim:

> | `DTSTART` | preserved only locally | never folded into `due` | `google_tasks::policy::tests::dtstart_survives_the_push_and_is_never_folded_into_due` |

`:176` — the `DUE` row:

> | `DUE` (datetime) | lossy | comes back date-only, per `P5-05`'s canonicalization (`google_tasks/due.rs`); this ticket only classifies the loss, not the conversion | ... |

`:37-41` — why:

> `due` field time component: per the Tasks resource reference, **"the due date only records date
> information; the time portion of the timestamp is discarded when setting the due date"** and it is
> not possible to read or write the time a task is due via the API

### The user-facing statement

`website/src/content/docs/providers.md:56`:

> Google Tasks is a lossy remote: several ICS fields caldir models are kept locally but never reach
> Google (`PRIORITY`, `URL`, `DTSTART`, `PERCENT-COMPLETE`, attendees, alarms, attachments, and
> unknown `X-` properties), a recurring task (`RRULE`) is refused outright rather than silently
> dropped, and a task's `DUE` datetime comes back as a date.

`providers.md:50` — a Google **Calendar** remote never takes a task:

> Google keeps tasks in a separate product (Google Tasks) with its own API. A Google Calendar remote
> is never reinterpreted as a task list, so `caldir push` and `caldir sync` hold a task change back
> and print a note saying it wasn't pushed.

### Spec

`docs/vtodo/spec.md:181` classifies `DTSTART` as **IN (`Option`)** — *"Real producers emit both;
gives R44 a real anchor."* `spec.md:152`: *"A task MAY have `DTSTART`, `DUE`, both, or neither. All
four are first-class; none may be normalized away or given a synthesized value."*

### A relevant open question about u_crawler itself

`docs/vtodo/BACKLOG.md:528-538`, **Q-9 — OPEN, UNVERIFIED**:

> `DTSTART` decides whether the demo case is anchored or undated ... **Verified state at 902/0:** the
> fixture in `docs/vtodo/reproduction.md:14-33` is explicitly *"representative of"* u_crawler output,
> not captured from it — it carries `DUE` and **no** `LAST-MODIFIED` and **no** `DTSTART`.
> `find /home/belcaik/Dev/u_crawler -name '*.ics'` returns **nothing** ...

So caldir's own record is that u_crawler does **not** currently emit `DTSTART`. Adding it is new
territory for the fixtures, though not for the code — the codec handles it and the policy classifies
it.

### The transferable testing rule

`docs/vtodo/BACKLOG.md:292-299`, worth quoting for anyone writing round-trip tests against caldir:

> `MockProvider::reply` encodes the expected value using `to_ics_string` — **the encoder under
> test** — so a lossy serialize cancels on both sides and the assertion goes blind. **Measured:**
> under a `reply`-based idempotence gate, **6 of 12** mutations that the raw-fixture version catches
> SURVIVED (dropped `COMPLETED`, stripped `X-*`, flattened `TZID`, coerced floating `COMPLETED`,
> synthesized `DTSTART`, `DESCRIPTION` drift). **Every future round-trip / idempotence / fidelity
> assertion must drive the remote from RAW ICS via `reply_raw_data`, never from domain values via
> `reply`.**

---

## 6. The concrete scenario

**Input.** `u_crawler` writes a VTODO carrying:

```
UID:u_crawler-todo-9001571@u-crawler.local
DTSTAMP:20260610T191757Z
DTSTART:20260826T040000Z          <- unlock_at
DUE:20260902T035959Z              <- due_at
SUMMARY:Semana 11: Sumativa 5: Solemne 2
DESCRIPTION:Line one\nLine two\, with a comma\nLine three
URL:https://lms.example.edu/courses/900223/assignments/9001571
PRIORITY:1
```

### (a) caldir local parse → serialize round trip

**Everything above survives, structurally and (for the properties tested) byte-for-byte.**

| property | outcome |
|---|---|
| `UID` | preserved verbatim |
| `DTSTAMP` | **replaced** with a fresh stamp on every write (`to_icalendar.rs:116-118`). Byte comparison of two generations will always differ here; caldir's own tests compare parsed domain values, never bytes |
| `DTSTART:20260826T040000Z` | → `EventTime::DateTimeUtc` → re-emitted as `DTSTART:20260826T040000Z` (`to_icalendar.rs:20-22`, byte form asserted at `:298`) |
| `DUE:20260902T035959Z` | → `EventTime::DateTimeUtc` → re-emitted as `DUE:20260902T035959Z` (`:24-26`, `:302`) |
| `SUMMARY` | verbatim, incl. non-ASCII and unescaped colons |
| `DESCRIPTION` (multi-line, escaped) | preserved as a Rust `String` with real newlines, re-escaped and re-folded by the crate on write. Byte-symmetric on the event path (`event.rs:458`); **not independently pinned for VTODO** — see §2.4 |
| `URL` | verbatim (`to_icalendar.rs:104-106`). Comma/semicolon escaping in a URI value is **UNVERIFIED** |
| `PRIORITY:1` | verbatim, raw, unclamped |

Property **ordering** is the crate's, not the input's, so a naive byte-diff of input vs output will
show reordering as well as the new `DTSTAMP`.

Filename: the file is named `_task__2026-09-01T2359__semana-11-sumativa-5-solemne-2.ics` — the slug
anchor is `DUE`, falling back to `DTSTART` (`todo/slugify.rs:16-33`), rendered in the **local**
timezone by `time_slug`.

**Silently dropped in the local round trip:** nothing from the input above. But if `u_crawler` also
emitted `CATEGORIES`, `RELATED-TO`, `GEO`, `RESOURCES`, `COMMENT`, `CONTACT`, `CLASS`,
`REQUEST-STATUS` or any other unmodelled IANA/vendor property, **all of it dies on caldir's first
write** — there is no passthrough bag beyond `X-`.

### (b) push to Google Tasks

Wire body sent to `POST https://tasks.googleapis.com/tasks/v1/lists/{id}/tasks`:

```json
{
  "title":  "Semana 11: Sumativa 5: Solemne 2",
  "notes":  "Line one\nLine two, with a comma\nLine three",
  "status": "needsAction",
  "due":    "2026-09-01T00:00:00.000Z"
}
```

(`due` computed as `google_due_date(DUE, chrono::Local)` — in `America/Santiago`,
`2026-09-02T03:59:59Z` is `2026-09-01`. On a machine running `TZ=UTC` the same input would send
`2026-09-02`. The date the user sees **depends on the syncing machine's timezone**.)

**Not on the wire at all:** `DTSTART`, `URL`, `PRIORITY`, `UID`, `DTSTAMP`, `LOCATION`,
`PERCENT-COMPLETE`, attendees, organizer, alarms, attachments, other `X-` properties.

Then Google's response is merged and **the local `.ics` file is rewritten with the merge result**
(`connection.rs:267-271`). Net effect on disk:

| property | after the push |
|---|---|
| `DTSTART:20260826T040000Z` | **unchanged** — carried forward from local (`policy.rs:285-299`) |
| `URL`, `PRIORITY:1` | **unchanged** — carried forward from local (`policy.rs:246-281`) |
| `UID` | **unchanged** — local UID kept (`mod.rs:63-80`) |
| `SUMMARY` | replaced by Google's `title` echo (identical unless Google normalizes it) |
| `DESCRIPTION` | replaced by Google's `notes` echo |
| **`DUE:20260902T035959Z`** | **replaced by `DUE;VALUE=DATE:20260901`** — the time of day is gone, permanently, from the local file too |
| filename | **renamed** from `_task__2026-09-01T2359__…` to `_task__2026-09-01__…` (the slug anchor changed shape), and the old file is `remove_file`d (`calendar/event.rs:57-77`) |
| new `X-GOOGLE-TASK-ID:<id>` | added |
| `DTSTAMP` | re-stamped |

### What would be silently dropped or mangled — the honest list

1. **`DTSTART` never reaches Google.** A user looking at Google Tasks sees only the deadline, never
   the unlock date. This is documented, tested and intentional — but it means DTSTART is dead weight
   for the Google-Tasks destination. It *does* survive locally and it *does* reach a CalDAV remote.
2. **`DUE`'s time-of-day is destroyed, and the destruction propagates back into the local file.**
   This is the one genuinely lossy write. It is not recoverable: the next pull reads the date-only
   value as canonical. `a_timestamped_due_is_classified_lossy_not_faithful` (`policy.rs:167-185`)
   pins exactly this.
3. **The `due` calendar date is computed in `chrono::Local`.** Two machines in different zones
   syncing the same directory will disagree about which day a `T035959Z` deadline lands on. There is
   no configuration knob; `create_event.rs:72` and `update_event.rs:80` hard-code `chrono::Local`.
4. **`URL` and `PRIORITY` are invisible on Google** — the two properties on all 57 production tasks.
   The doc calls this out explicitly (`google-tasks.md:150-151`).
5. **A PATCH with `description = None` sends `"notes": null` and clears Google's notes**
   (`to_google.rs:34-45`). If someone edits notes in the Google Tasks app and the local file has no
   `DESCRIPTION`, the next push wipes their edit.
6. **An unknown `STATUS` value is silently dropped to `NEEDS-ACTION`** and is *not* smuggled into the
   `X-` bag — a documented loss, pinned at `todo.rs:219-237`.
7. **Adding `RRULE` would make the push fail outright** with a `SemanticLimitation` error (loudly,
   not silently — this is the good failure mode).
8. **Any property outside the modelled set + `X-` is destroyed on the first caldir write.** If
   `u_crawler` wants extra data to survive, it must use an `X-` property. Note the known `X-` loss:
   repeated same-name `X-` properties collapse to one (`spec.md:195`).
9. **A VTODO pushed at a Google *Calendar* remote is held back**, not converted — the remote
   advertises `tasks: {read:false, write:false}`.
10. **Comma/semicolon escaping in `URL`** — the pinned crate demonstrably escapes commas in property
    values (`spec.md:197`, verified for `CATEGORIES`). Whether it does so for the URI-typed `URL` is
    untested in caldir and unverified here. If it does, a Canvas URL with a comma in the query string
    would be written as `URL:...\,...` — round-trip-safe within caldir, non-conformant for third
    parties. **Worth a one-line test before relying on it.**

---

## 7. How to run caldir's tests

`just test` → **`cargo test`**, run from the workspace root (`justfile`). It is a virtual workspace
(`Cargo.toml` members: `caldir-cli`, `caldir-core`, and the five provider crates), so bare
`cargo test` compiles and runs every crate's tests. No feature flags are involved.

```sh
cd <caldir checkout>
cargo test                       # everything
cargo test -p caldir-core todo   # just the VTODO codec
cargo test -p caldir-provider-google google_tasks
```

Toolchain is pinned: `rust-toolchain.toml` → `channel = "1.97.0"`.

**Credentials / network: not needed.** Every test that touches a real service is gated at
**runtime** on an env var, never `#[ignore]` (deliberately — see the doc comment at
`caldir-provider-google/src/live/google_tasks.rs:1-31`). With the gates unset, those tests run as
no-ops and the suite is green offline:

- `CALDIR_LIVE_GOOGLE=1` + `CALDIR_LIVE_GOOGLE_ACCOUNT` + `CALDIR_LIVE_GOOGLE_TASK_LIST` — real
  Google account, real disposable task list.
- `CALDIR_LIVE_RADICALE` — a throwaway Radicale container (`caldir-provider-caldav/src/live/radicale.rs:120`).

The live Google invocation, verbatim from that doc comment:

```sh
CALDIR_LIVE_GOOGLE=1 CALDIR_LIVE_GOOGLE_ACCOUNT=you@gmail.com \
  CALDIR_LIVE_GOOGLE_TASK_LIST=<id> \
  cargo test -p caldir-provider-google -- live::google_tasks
```

**Timezone matters.** CI runs the whole suite three times, under `TZ=UTC`,
`TZ=Europe/Stockholm` and `TZ=America/New_York` (`.github/workflows/ci.yml:33-52`), because a large
share of the VTODO/DUE logic is zone-sensitive. Reproduce a CI failure with e.g.
`TZ=America/New_York cargo test`. CI also sets `RUSTFLAGS=-D warnings` and runs
`just check` (`cargo check --workspace` + `cargo clippy --workspace -- -D warnings`) and
`cargo fmt --all -- --check`.

*(Note: the local machine's cargo registry does not have `icalendar 0.17.10` cached, so a first
`cargo test` here will need network access to fetch dependencies.)*

---

## Sources

All paths relative to the read-only clone at
`/tmp/claude-1000/-home-belcaik-Dev-unab-sync-content/0c205e29-1023-4b26-a55a-e7a544127f53/scratchpad/deps/caldir`
(branch `vtodo-support`, HEAD `02ee000`).

**Model & codec**
- `caldir-core/src/todo.rs` — `Todo` struct (`:28-79`), `occurs_in_range` (`:127-143`),
  `to_ics_string` (`:148-159`), VALARM splice (`:165-173`), loss-set pins (`:705-901`)
- `caldir-core/src/todo/from_icalendar.rs` — parser (`:9-118`), `present_or_unparseable` (`:155-167`)
- `caldir-core/src/todo/to_icalendar.rs` — serializer (`:13-122`)
- `caldir-core/src/todo/slugify.rs` — filename anchor (`:16-33`)
- `caldir-core/src/todo/status.rs` — `TodoStatus`
- `caldir-core/src/event/time.rs` — `EventTime` (`:5-14`), `DatePerhapsTime` conversions (`:90-145`)
- `caldir-core/src/item.rs` — `CalendarItem`, one-ordered-pass parsing
- `caldir-core/src/test_utils.rs` — VTODO fixtures (`:240-360`)
- `caldir-core/src/connection.rs` — `push_outgoing_changes` (`:250-277`), idempotence gate (`:~2250-2340`)
- `caldir-core/src/calendar/event.rs` — `update` and the rename-on-slug-change (`:51-79`)
- `caldir-core/src/event.rs` — the folded/escaped DESCRIPTION byte round trip (`:412-459`)

**Google Tasks provider**
- `caldir-provider-google/src/google_tasks/mod.rs`
- `caldir-provider-google/src/google_tasks/client.rs`
- `caldir-provider-google/src/google_tasks/to_google.rs`
- `caldir-provider-google/src/google_tasks/from_google.rs`
- `caldir-provider-google/src/google_tasks/due.rs`
- `caldir-provider-google/src/google_tasks/policy.rs`
- `caldir-provider-google/src/commands/create_event.rs` — `task_insert_request` (`:68-75`),
  `merge_canonical_task_response` (`:83-117`)
- `caldir-provider-google/src/commands/update_event.rs` — `task_patch_request` (`:72-83`)
- `caldir-provider-google/src/commands/delete_event.rs`
- `caldir-provider-google/src/commands/list_events.rs` — `process_google_tasks` (`:99-117`)
- `caldir-provider-google/src/commands/capabilities.rs`
- `caldir-provider-google/src/remote_config.rs` — `GoogleResourceKind` (`:65-100`)
- `caldir-provider-google/src/live/google_tasks.rs`

**Docs**
- `docs/vtodo/provider-sync/providers/google-tasks.md` — the authoritative mapping + loss table
- `docs/vtodo/spec.md` — §6 property table (`:176-201`), R15 (`:206-210`), RFC 4791 §9.9 (`:392-432`)
- `docs/vtodo/BACKLOG.md` — the raw-ICS rule (`:292-299`), Q-9 (`:528-538`)
- `docs/vtodo/verification.md` — requirement-by-requirement pass table
- `website/src/content/docs/providers.md:48-58` — the user-facing Google Tasks limitation statement
- `website/src/content/docs/commands.md:117-132`, `overview.md:34-42` — task UX

**Build / CI**
- `Cargo.toml`, `Cargo.lock` (`icalendar 0.17.10`), `rust-toolchain.toml` (`1.97.0`), `justfile`,
  `.github/workflows/ci.yml`
