# Google Tasks REST API v1 — start date vs. deadline, API vs. UI

Research date: **2026-08-28**. All sources fetched on that date.
Primary machine-readable source: the live Discovery Document
`https://tasks.googleapis.com/$discovery/rest?version=v1`, **revision `20260825`**
(2026-08-25 — three days old at time of writing). Human docs page
`https://developers.google.com/workspace/tasks/reference/rest/v1/tasks` shows
**Last updated: 2026-02-24 UTC**. The two agree word-for-word on every field
description, so the docs page is current, not stale.

---

## Verdict

The hypothesis is **CONFIRMED on the load-bearing point, and REFUTED on the naming/semantics**.
Both halves matter, and the second half changes how a VTODO should be projected.

**Confirmed:** The public Google Tasks API v1 exposes exactly **one writable
date field on a task: `due`**. It records **date only** — the time-of-day is
explicitly discarded on write and cannot be read back. There is **no**
`startDate`, `start`, `deadline`, `scheduledDate`, `taskDate`, `duration`,
`endTime` or any other date/time field in the Task schema, writable or
read-only. The complete Task schema is 15 properties plus a read-only
`assignmentInfo` object; that is the entire surface. Meanwhile the end-user UI
**does** expose two distinct concepts — "Start date and time" (with a duration)
and "Deadline" — neither of which is individually addressable through the
public API.

**Refuted (important):** Google has **rewritten the `due` field's
documentation**. The historical wording ("Due date of the task…") is **gone**.
The current text says `due` is the **scheduled** date and explicitly states:

> "It doesn't represent the deadline of the task."

So `due` is no longer documented as a deadline at all. It is documented as *the
day the task is scheduled / visible on the calendar grid* — i.e. it maps to the
UI's **start date**, not the UI's **Deadline**. An iCalendar VTODO's `DUE`
written into `task.due` will be interpreted by Google as the task's *scheduled
day*, and will render in Calendar at the start-date position, not in the
Deadline slot.

**Consequence for VTODO projection:** a VTODO carrying both `DTSTART` and `DUE`
**cannot** be faithfully projected. One of the two must be chosen for `due`
(semantically `DTSTART` is the closer match to the current documented meaning of
`due`), any time-of-day on either is lost, and the other date can only be
preserved as human-readable text in `notes` — with no machine round-trip.

---

## Q1 — Every field of the Task resource

JSON representation, verbatim from
<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks>
(fetched 2026-08-28):

```json
{
  "kind": string,
  "id": string,
  "etag": string,
  "title": string,
  "updated": string,
  "selfLink": string,
  "parent": string,
  "position": string,
  "notes": string,
  "status": string,
  "due": string,
  "completed": string,
  "deleted": boolean,
  "hidden": boolean,
  "links": [
    {
      "type": string,
      "description": string,
      "link": string
    }
  ],
  "webViewLink": string,
  "assignmentInfo": {
    object (AssignmentInfo)
  }
}
```

That is the **complete** list. There is nothing else.

"Writable?" below is taken from the Discovery Document's `readOnly: true` flag
(authoritative, machine-readable, revision `20260825`), cross-checked against the
"Output only." prose in the reference page.

| Field | Type | `readOnly` in discovery | Verbatim description |
|---|---|---|---|
| `kind` | string | **true** | "Output only. Type of the resource. This is always \"tasks#task\"." |
| `id` | string | *(absent — writable in payload, server-assigned)* | "Task identifier." |
| `etag` | string | *(absent)* | "ETag of the resource." |
| `title` | string | *(absent — **writable**)* | "Title of the task. Maximum length allowed: 1024 characters." |
| `updated` | string | **true** | "Output only. Last modification time of the task (as a RFC 3339 timestamp)." |
| `selfLink` | string | **true** | "Output only. URL pointing to this task. Used to retrieve, update, or delete this task." |
| `parent` | string | **true** | "Output only. Parent task identifier. This field is omitted if it is a top-level task. Use the \"move\" method to move the task under a different parent or to the top level. A parent task can never be an assigned task (from Chat Spaces, Docs). This field is read-only." |
| `position` | string | **true** | "Output only. String indicating the position of the task among its sibling tasks under the same parent task or at the top level. If this string is greater than another task's corresponding position string according to lexicographical ordering, the task is positioned after the other task under the same parent task (or at the top level). Use the \"move\" method to move the task to another position." |
| `notes` | string | *(absent — **writable**)* | "Notes describing the task. Tasks assigned from Google Docs cannot have notes. Optional. Maximum length allowed: 8192 characters." |
| `status` | string | *(absent — **writable**)* | "Status of the task. This is either \"needsAction\" or \"completed\"." |
| `due` | string | *(absent — **writable**)* | see Q2 |
| `completed` | string | *(absent — **writable**)* | "Completion date of the task (as a RFC 3339 timestamp). This field is omitted if the task has not been completed." |
| `deleted` | boolean | *(absent — **writable**, except on assigned tasks)* | "Flag indicating whether the task has been deleted. For assigned tasks this field is read-only. They can only be deleted by calling tasks.delete, in which case both the assigned task and the original task (in Docs or Chat Spaces) are deleted. To delete the assigned task only, navigate to the assignment surface and unassign the task from there. The default is False." |
| `hidden` | boolean | *(absent in discovery, but prose says read-only)* | "Flag indicating whether the task is hidden. This is the case if the task had been marked completed when the task list was last cleared. The default is False. **This field is read-only.**" |
| `links[]` | object[] | **true** | "Output only. Collection of links. This collection is read-only." |
| `links[].type` | string | (inside read-only array) | "Type of the link, e.g. \"email\", \"generic\", \"chat_message\", \"keep_note\"." |
| `links[].description` | string | (inside read-only array) | "The description (might be empty)." |
| `links[].link` | string | (inside read-only array) | "The URL." |
| `webViewLink` | string | **true** | "Output only. An absolute link to the task in the Google Tasks Web UI." |
| `assignmentInfo` | object (AssignmentInfo) | **true** | "Output only. Context information for assigned tasks. A task can be assigned to a user, currently possible from surfaces like Docs and Chat Spaces. This field is populated for tasks assigned to the current user and identifies where the task was assigned from. This field is read-only." |

`AssignmentInfo` (all sub-fields `readOnly: true`), from the discovery document,
verbatim:

- `linkToTask` (string): "Output only. An absolute link to the original task in the surface of assignment (Docs, Chat spaces, etc.)."
- `surfaceType` (enum `CONTEXT_TYPE_UNSPECIFIED | GMAIL | DOCUMENT | SPACE`): "Output only. The type of surface this assigned task originates from. Currently limited to DOCUMENT or SPACE."
- `spaceInfo` (SpaceInfo): "Output only. Information about the Chat Space where this task originates from. This field is read-only."
- `driveResourceInfo` (DriveResourceInfo): "Output only. Information about the Drive file where this task originates from. Currently, the Drive file can only be a document. This field is read-only."

**Net writable set: `title`, `notes`, `status`, `due`, `completed`, `deleted`
(plus `id`/`etag` housekeeping, and `parent`/`position` only via the separate
`tasks.move` method).**

The other schemas in the whole API are only `TaskList`, `TaskLists`, `Tasks`,
`AssignmentInfo`, `SpaceInfo`, `DriveResourceInfo` — no date-bearing type is
hiding elsewhere.

---

## Q2 — The `due` field, verbatim

**Current text (identical in the reference page and in discovery revision `20260825`):**

> "Scheduled date for the task (as an RFC 3339 timestamp). Optional. This represents the day that the task should be done, or that the task is visible on the calendar grid. It doesn't represent the deadline of the task. Only date information is recorded; the time portion of the timestamp is discarded when setting this field. It isn't possible to read or write the time that a task is scheduled for using the API."

Source: <https://developers.google.com/workspace/tasks/reference/rest/v1/tasks>
(Last updated 2026-02-24 UTC) and
`https://tasks.googleapis.com/$discovery/rest?version=v1` (revision 20260825).
Both fetched 2026-08-28.

**Has the historical wording survived? No — it has been rewritten.** The old
text quoted in the brief ("Due date of the task (as a RFC 3339 timestamp)… the
due date only records date information; the time portion of the timestamp is
discarded when setting the due date. It isn't possible to read or write the time
that a task is due via the API.") is **no longer present**. The substantive
change:

| Old (historical) | Current (2026) |
|---|---|
| "**Due** date of the task" | "**Scheduled** date for the task" |
| — | "This represents the day that the task should be done, or that the task is visible on the calendar grid." |
| — | "**It doesn't represent the deadline of the task.**" |
| "the time portion of the timestamp is discarded when setting the due date" | "Only date information is recorded; the time portion of the timestamp is discarded when setting this field." |
| "read or write the time that a task is **due**" | "read or write the time that a task is **scheduled for**" |

**Time-of-day: NOT preserved.** Stated twice and unambiguously — the time
portion is *discarded on write*, and it is *not possible to read or write* the
time via the API. You must still send a syntactically valid RFC 3339 timestamp
(e.g. `2026-09-14T00:00:00.000Z`); everything after the date is dropped. There
is no timezone handling to rely on either — the field carries a date, not an
instant.

The list filters `dueMin` / `dueMax` are likewise described only in terms of
"a task's due date (as a RFC 3339 timestamp)", with no time granularity claim:
<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/list>
(Last updated 2025-04-10 UTC).

---

## Q3 — Is there a writable "start date" or a separate "deadline" field?

**No. Definitively no, for both.**

Searched the complete Task schema in discovery revision `20260825` for
`startDate`, `start`, `deadline`, `scheduledDate`, `taskDate`, `duration`,
`endTime`, `startTime`. **Zero matches.** The Task object's property set is
exhaustively: `etag, links, due, webViewLink, completed, id, parent, notes,
updated, assignmentInfo, status, hidden, kind, selfLink, position, deleted,
title`. Nothing else exists to write.

The API's own release notes contain **no** entry adding any date field:
<https://developers.google.com/workspace/tasks/release-notes> lists only

> "July 23, 2024 — You can now get, edit, and delete tasks assigned from Google Docs documents or Chat spaces using the Tasks API."

and

> "June 28, 2018 — The Google Tasks API is now generally available."

(fetched 2026-08-28). So no new date/time field has been shipped publicly.

**The subtlety that refutes the naive reading of the hypothesis:** the single
writable date field is not "the deadline field". Google documents it as the
**scheduled** date and explicitly disclaims deadline semantics
("It doesn't represent the deadline of the task."). So the mapping is not
"API `due` == UI Deadline, and start date is missing"; it is closer to
**"API `due` == UI start *date* (day granularity only), and BOTH the start
*time*/duration AND the Deadline are missing."**

**(c) Internal/undocumented API:** the Google Tasks first-party web and mobile
clients evidently persist start time, duration and deadline (the UI shows and
round-trips them), so a non-public internal endpoint must carry them. Its
existence is inferable, but it is undocumented, unsupported, not covered by any
published contract, and outside the OAuth scopes below. **No details on using it
are given here, and it must not be relied on.**

---

## Q4 — What the end-user UI documents

Yes — the current Google help documentation describes **"Start date and time"**
and **"Deadline"** as two separate, simultaneously-settable user-facing fields.

From "Create & manage tasks in Google Calendar", Google Tasks Help /
Google Calendar Help,
<https://support.google.com/tasks/answer/9901136> and its Calendar mirror
<https://support.google.com/calendar/answer/9901136?hl=en&co=GENIE.Platform%3DDesktop>
(fetched 2026-08-28; page footer "©2026 Google", no explicit last-updated stamp):

> **"Start date and time"** — "Enter the start date and how much time you plan to spend on the task."

> **"Deadline"** — "Enter the date you expect to complete the task. The deadline appears on the 'All day' section of Calendar."

> "Add time" — used to "convert an existing task into a task with a start and end time."

> "To repeat a task, select the date and time [and then] Repeat"

From "Add or edit a task", <https://support.google.com/tasks/answer/7675838?hl=en>
(fetched 2026-08-28), same two labels, plus notification behaviour:

> "For tasks without a time, notifications appear at 9 AM."

> a notification "to complete the task on the day of the deadline at 9 AM in your local time"

So the UI's model is richer than the API's in three ways at once: a **start
time**, a **duration/end time**, and a **separate Deadline date**. The Deadline
in the UI is date-only (no time-of-day option), matching the reporting in the
consumer press around its rollout.

Clean separation of the three layers:

- **(a) Public documented API** — one writable date, `due`, day-granularity, documented as *scheduled date*, explicitly *not* the deadline. No start time, no duration, no deadline.
- **(b) End-user UI** — "Start date and time" (date + time + duration) *and* "Deadline" (date only), as two independent fields, per the support pages quoted above.
- **(c) Internal/undocumented API** — must exist to back (b); not documented, not supported, not usable within the published scopes. Existence noted only.

Related official Calendar-side rollout, "Block off time to work on a task in
Calendar", Google Workspace Updates, posted **November 17, 2025**
(<https://workspaceupdates.googleblog.com/2025/11/block-time-for-tasks-google-calendar.html>,
fetched 2026-08-28):

> "Users can now easily block off time on their calendar to work on a specific task."
> "You'll also see the task on your task list and get reminded until the task is completed."

Rollout: Rapid Release from November 6, 2025; Scheduled Release from
December 1, 2025. Available to all Workspace customers, Workspace Individual
subscribers, and personal Google accounts. The post announces a **UI**
capability; it announces no API field, and none appears in the API release notes.

---

## Q5 — How `due` interacts with the Calendar grid

The authoritative statement is inside the API doc for `due` itself
(<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks>):

> "This represents the day that the task should be done, or **that the task is visible on the calendar grid**."

So: a task written via the API appears in Google Calendar on the **`due` date**,
as an all-day/undated item on that day. There is no way to place it at a time
slot via the API, because the time is discarded.

The help page adds the UI-side placement rules
(<https://support.google.com/calendar/answer/9901136?hl=en&co=GENIE.Platform%3DDesktop>):

> Deadline: "The deadline appears on the 'All day' section of Calendar."

> "a certain number of upcoming instances of a repeating task will appear on the calendar grid, and as time passes, new ones will be added automatically."

Tasks given a start date **and time** in the UI render in the timed grid at that
slot; tasks without a time render as all-day. Since the API cannot set a time,
**every API-created task lands in the all-day row of its `due` date.**

---

## Q6 — The `notes` field

**Max length — 8192 characters**, verbatim
(<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks>):

> "Notes describing the task. Tasks assigned from Google Docs cannot have notes. Optional. Maximum length allowed: 8192 characters."

Note the additional constraint: **tasks assigned from Google Docs cannot have
notes at all.** (For comparison, `title` is capped at 1024 characters.)

**Newlines:** there is **no official statement** in the reference, the discovery
document, or the help pages about newline handling in `notes`. The field is
typed only as `string` with a length cap. Searches of developers.google.com and
support.google.com surfaced nothing. **Unverified — do not assume; test
empirically before relying on multi-line notes.**

**URL linkification:** likewise **no official statement** anywhere in the
primary sources that URLs placed in `notes` are auto-linked in any client. The
only documented link mechanism is the read-only `links[]` collection (Q7).
**Unverified.**

---

## Q7 — Is `links` read-only, and is it Gmail-only?

**Read-only: yes, stated explicitly and flagged in the machine-readable
schema.** Verbatim:

> "Output only. Collection of links. This collection is read-only."

and in discovery revision `20260825` the `links` array carries `"readOnly": true`.
It cannot be written on insert or patch.

**Gmail-only: no — that is refuted by the current docs.** The link `type` is
documented as:

> "Type of the link, e.g. \"email\", \"generic\", \"chat_message\", \"keep_note\"."

So links are populated from at least Gmail (`email`), Chat (`chat_message`),
Keep (`keep_note`), and a `generic` catch-all — not Gmail alone. There is no
statement anywhere that a client may supply them; the only documented
provenance is Google's own surfaces. **Implication: you cannot attach a source
URL to a task as a structured link. A URL can only go in `notes` (as plain
text, with linkification unverified — see Q6).**

---

## Q8 — OAuth scopes to write tasks

From <https://developers.google.com/workspace/tasks/auth>, and confirmed
per-method in the discovery document (fetched 2026-08-28):

| Scope | Verbatim description |
|---|---|
| `https://www.googleapis.com/auth/tasks` | "Create, edit, organize, and delete all your tasks." |
| `https://www.googleapis.com/auth/tasks.readonly` | "View your tasks." |

**To write, you need `https://www.googleapis.com/auth/tasks`.** It is the *only*
scope accepted by the mutating methods:

- `tasks.insert` — "Requires: `https://www.googleapis.com/auth/tasks`" (<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/insert>, Last updated 2025-03-13 UTC); discovery confirms `"scopes": ["https://www.googleapis.com/auth/tasks"]`.
- `tasks.patch` — same single scope (<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/patch>, Last updated 2025-03-13 UTC).
- `tasks.list` — accepts both `.../auth/tasks` and `.../auth/tasks.readonly` (<https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/list>, Last updated 2025-04-10 UTC).

Relevant quotas, verbatim from the insert page:

> "A user can have up to 20,000 non-hidden tasks per list and up to 100,000 tasks in total at a time."

And from the discovery description of `tasks.insert`:

> "Tasks assigned from Docs or Chat Spaces cannot be inserted from Tasks Public API; they can only be created by assigning them from Docs or Chat Spaces."

`tasks.patch` supports patch semantics, so partial updates (e.g. `due` alone)
are fine; both `insert` and `patch` take a full `Task` resource as the body.

---

## Concept → public API field → writable? → fidelity

| Concept (VTODO / UI) | Public API field | Writable? | Fidelity |
|---|---|---|---|
| VTODO `SUMMARY` | `title` | Yes | **Full**, ≤1024 chars |
| VTODO `DESCRIPTION` | `notes` | Yes | **Good**, ≤8192 chars; newline behaviour undocumented |
| VTODO `STATUS` (`NEEDS-ACTION` / `COMPLETED`) | `status` | Yes | **Full** — only two values exist |
| VTODO `COMPLETED` (timestamp) | `completed` | Yes | RFC 3339; no doc statement that time is discarded here |
| VTODO `DTSTART` (date part) | `due` — *if you choose to map it here* | Yes | **Lossy**: date only. Semantically the closest match to the current doc ("scheduled date… visible on the calendar grid") |
| VTODO `DTSTART` (time-of-day) | — | **No** | **LOST.** "the time portion of the timestamp is discarded" |
| VTODO start *duration* / end time | — | **No** | **LOST.** UI-only ("how much time you plan to spend on the task") |
| VTODO `DUE` as a true deadline | — | **No field.** `due` explicitly "doesn't represent the deadline of the task" | **LOST** as a distinct concept; can only be flattened into `due` (changing its meaning) or written as text in `notes` |
| Both `DTSTART` **and** `DUE` together | — | **No** | **IMPOSSIBLE.** One writable date field; the other date survives only as prose in `notes`, with no machine round-trip |
| VTODO `PRIORITY` | — | No | Not modelled at all |
| VTODO `RRULE` / recurrence | — | No | UI-only ("Repeat"); no API field |
| VTODO `URL` / source link | `links[]` | **No — output only** | Must be embedded in `notes` as text; linkification unverified |
| VTODO `PERCENT-COMPLETE` | — | No | Not modelled |
| VTODO `CATEGORIES` | — | No | Not modelled |
| Sub-task nesting (`RELATED-TO`) | `parent` | Output only — set via `tasks.move` | Achievable, but only through a separate call |
| Ordering | `position` | Output only — set via `tasks.move` | Same |
| Deletion | `deleted` | Yes (not for assigned tasks) | Full |

---

## Sources

All fetched **2026-08-28**.

1. **REST Resource: tasks** — <https://developers.google.com/workspace/tasks/reference/rest/v1/tasks> — Last updated 2026-02-24 UTC. Task JSON representation, complete field table, `due` / `notes` / `links` / `assignmentInfo` descriptions.
2. **Live Discovery Document** — `https://tasks.googleapis.com/$discovery/rest?version=v1` — **revision `20260825`**. Machine-readable, authoritative `readOnly` flags and the exhaustive property set. Verified byte-for-byte agreement with source 1 on `due`.
3. **Method: tasks.insert** — <https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/insert> — Last updated 2025-03-13 UTC. Endpoint, `parent`/`previous` query params, scope, 20,000/100,000 quotas.
4. **Method: tasks.patch** — <https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/patch> — Last updated 2025-03-13 UTC. Patch semantics, scope.
5. **Method: tasks.list** — <https://developers.google.com/workspace/tasks/reference/rest/v1/tasks/list> — Last updated 2025-04-10 UTC. `dueMin`/`dueMax`/`showCompleted`/`showHidden`/`showDeleted`/`showAssigned`, both scopes.
6. **Google Tasks API release notes** — <https://developers.google.com/workspace/tasks/release-notes> — only 2018-06-28 (GA) and 2024-07-23 (assigned tasks). No date-field additions.
7. **Choose Google Tasks API scopes / auth** — <https://developers.google.com/workspace/tasks/auth> — the two OAuth scopes and their verbatim descriptions.
8. **Create & manage tasks in Google Calendar** (Google Tasks Help) — <https://support.google.com/tasks/answer/9901136> — "Start date and time" and "Deadline" as separate UI fields; "Add time"; ©2026 Google.
9. **Create & manage tasks in Google Calendar** (Google Calendar Help mirror) — <https://support.google.com/calendar/answer/9901136?hl=en&co=GENIE.Platform%3DDesktop> — deadline in the "All day" section; repeating instances on the grid.
10. **Add or edit a task** (Google Tasks Help) — <https://support.google.com/tasks/answer/7675838?hl=en> — same two labels; 9 AM notification behaviour for untimed tasks and for deadlines.
11. **Block off time to work on a task in Calendar** — Google Workspace Updates, posted 2025-11-17 — <https://workspaceupdates.googleblog.com/2025/11/block-time-for-tasks-google-calendar.html> — UI-only capability; rollout Nov 6 / Dec 1 2025.

### Fetch failures / gaps

- No fetch failed. Every URL in the brief resolved.
- **Gap:** no official Google source states how `notes` handles **newlines**, nor whether URLs in `notes` are **linkified**. Absence of documentation, not a documented "no". Verify empirically.
- **Gap:** Google Issue Tracker threads on this topic (e.g. 166896024 "Please update the API (set precise time, etc.)") require sign-in and were not readable; they are user-filed requests, not primary documentation, and nothing in them would change the schema evidence above.
- **Gap:** I found no Workspace Updates post announcing the Tasks *Deadline* feature itself; the feature is documented in the two support pages (sources 8–10), which is sufficient primary evidence for the UI claim. Consumer-press coverage of the deadline rollout exists but is not a primary source and is not cited as evidence here.
