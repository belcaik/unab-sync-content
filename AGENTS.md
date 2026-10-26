# AGENTS.md

## Project: `u_crawler` — Canvas/Zoom course backup CLI

A Rust CLI that authenticates to Canvas with a Personal Access Token, enumerates
courses/modules/files, and mirrors them to a structured folder tree. It also
captures a Zoom LTI session through a headless browser and downloads the class
recordings that session can reach.

> This document describes what the code **does**. If you change behaviour, change
> this file in the same commit. Statements here that the code contradicts are
> bugs in this file.

---

## Non-negotiables

* **Idempotent & resumable:** `Range` requests, `.part` files, ETag validation,
  atomic rename on finalize. A `200` in reply to a `Range` request means the
  server ignored it, and the partial file must be truncated rather than
  appended to — `download.rs` handles this; anything reimplementing it must.
* **Deterministic structure:** stable, sanitized, ASCII-transliterated paths.
* **`--dry-run` writes nothing and launches nothing.** It must not start a
  browser, perform SSO, or download.
* **Never log a credential value.** Log presence (`has_token = true`), never the
  token, scid, cookie or password. The log file is append-only for the life of
  the install.
* **Nothing outside `main.rs` writes to stdout.** Library modules emit `tracing`
  events; user-facing output goes through the `status!` macro in `ui`, which
  prints through the shared progress-bar group.
* **`u_crawler` is the exclusive owner of the directories it creates under
  `calendar.caldir_root`.** No other process writes there; `caldir push` reads
  and syncs it to Radicale but is not a writer of it (spec D4, D8).

---

## Tech Stack

* **Language:** Rust (Edition 2021)
* **Async runtime:** `tokio`
* **HTTP:** `reqwest` (gzip/brotli/deflate, streaming, rustls)
* **CLI:** `clap` (derive)
* **Config:** TOML (`directories`, `toml`)
* **Parsing:** `serde`, `serde_json`, `regex`, `url`, `html2md`
* **Storage:** `rusqlite` (Zoom session store), JSON (`state.json` per course)
* **Migration:** `blake3` (`pure`) for collision checks; `rustix` (Linux only)
  for `renameat2(RENAME_NOREPLACE)`, so a migration rename never replaces a file
* **Browser automation:** `chromiumoxide` (pinned to a git rev — see Cargo.toml).
  Gated behind the default-on `zoom` cargo feature, along with the rest of the
  Zoom flow (`rusqlite`, `cookie`, `cookie_store`, `reqwest_cookie_store`,
  `base64`, `futures`). `cargo build --no-default-features` drops all of
  these and needs neither network+git for `chromiumoxide` nor a browser —
  see "Building without Zoom" below.
* **Errors:** `thiserror` for typed module errors, `anyhow` for orchestration
* **UX:** `indicatif`
* **Process:** `tokio::process` (for `ffmpeg`)

---

## Repository Conventions

* **Branching:** `main` (default). Feature branches: `feat/<scope>-<short-desc>`.
* **Conventional Commits:** `feat:`, `fix:`, `docs:`, `refactor:`, `perf:`,
  `test:`, `build:`, `ci:`, `chore:`, `revert:`.
* **Formatting & Lint:** `cargo fmt --all` and
  `cargo clippy --all-targets --all-features --locked -- -D warnings`.
  `Cargo.toml` carries a `[lints.clippy]` table so local runs match CI.
* **Run `rustup update` before trusting a local clippy run.** CI installs the
  *latest* stable via `dtolnay/rust-toolchain@stable`, so a local toolchain a
  few releases behind will pass lints that CI then fails on — new clippy lints
  land every release. To reproduce CI exactly, run it:
  `act pull_request -j clippy -s GITHUB_TOKEN="$(gh auth token)"`.
* **No `#[allow(clippy::...)]`** without a comment justifying it. There are
  currently none in the crate; adding one is a design signal, not a fix.
* **Tests:** `cargo test`. Prefer pure functions that can be tested without a
  network or a browser — that is why `links`, `download`, `zoom::app_conf` and
  `zoom::sso` are separate from the flows that call them.

---

## Building without Zoom

The `zoom` cargo feature (default-on) gates `src/zoom/` in its entirety and
the `chromiumoxide` dependency that only it uses:

```bash
cargo build --release --no-default-features   # no browser, no git dependency
cargo test --no-default-features
cargo clippy --all-targets --no-default-features --locked -- -D warnings
```

With the feature off:

* `src/lib.rs` does not compile `pub mod zoom` at all.
* `sync` still runs, but the Zoom stage is a no-op (logged at `info`) instead
  of calling `zoom::zoom_flow`, regardless of `zoom.enabled` in the config.
* `zoom flow` still exists as a `clap` subcommand — it is not removed from
  `--help` — but its handler prints a clear "this build does not include the
  Zoom flow" message to stderr and exits `12`, rather than failing to parse or
  silently doing nothing.
* `tests/zoom_db.rs` is gated with `#![cfg(feature = "zoom")]` since it drives
  `zoom::db` directly and needs `rusqlite`, which is optional and only pulled
  in by the `zoom` feature.

This is what makes the calendar-sync flow buildable without `chromiumoxide`
(spec: `docs/specs/calendar-sync-flow.md`, "Docker" under Further Notes) — the
motivating case is a small, reproducible image for the calendar cron
container. Every other flow (`sync` with Zoom, `zoom flow`) is unaffected when
the feature is left on, which is the default.

The cron container itself is `Dockerfile` + `docker-compose.yml` +
`.env.example` + `docker/` at the repo root — see README.md "Docker / cron
deployment" for the full startup sequence, including the config-must-be-
mounted-first trap in `main.rs`.

---

## CLI Spec

### Binary

`u_crawler`

### Commands

| Command | Flags | Behaviour |
|---|---|---|
| `init` | — | Create the default config and paths. |
| `auth canvas` | `--base-url`, and one of `--token` / `--token-cmd` | Store the Canvas PAT, or a command that prints it. |
| `scan` | `--course-id` | Enumerate courses/modules/files. No writes. |
| `sync` | `--course-id`, `--dry-run`, `--verbose` | Mirror Canvas content, announcements and Zoom recordings. |
| `announcements` | `--course-id`, `--dry-run` | Announcements only: markdown bodies, extracted links and media, `index.json`. |
| `calendar` | `--course-id`, `--dry-run` | Project assignment deadlines to `.ics` `VTODO` files under `calendar.caldir_root`: course-labelled `SUMMARY`, three-line `DESCRIPTION`, `DTSTART`/`DUE`. |
| `recordings` | `--course-id`, `--dry-run` | Report Zoom links found across a course. Does not download. |
| `zoom flow` | `--course-id`, `--since` | Capture a Zoom session and download its recordings. |
| `migrate` | `--root`, `--dry-run`, `--undo <ts>`, `--canvas` | Migrate a layout v1 library to v2 (see "Migration"). `--root` migrates another tree and leaves the calendar alone; `--canvas` asks Canvas for names and module order. |
| `status` | `--verbose` | Per-course file counts, storage, last sync, failed items. |

`sync` honours `[announcements].enabled` and `[zoom].enabled`; either can be
turned off without touching the command line.

`sync`, `announcements`, `calendar` and `zoom flow` call
`migrate::ensure::ensure_configured` (→ `ensure_v2`) before they read the
library: a layout v1 library is migrated in place, always, with a `warn!`
summary (spec: "Layout viejo en `sync`"). A failed migration stops the
command with `12`, because writing v2 paths next to a half-migrated v1 tree
would download everything again. Their `--dry-run` only reports what the
migration would do.

### Exit Codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `10` | Config error, including "config was just created, go edit it" |
| `11` | Auth error |
| `12` | Network / rate-limit / runtime failure |
| `13` | `calendar` partial failure: at least one course synced and at least one failed. All courses failing is **not** this code — it is a hard failure and surfaces as `12` instead. |

`migrate` exits `0` on success (including "nothing to migrate"), `10` when
the config cannot be loaded, and `12` on any migration failure — the journal
holds what was done, so re-running resumes.

Codes 14–15 are not currently emitted. Do not document them until they are.

---

## Config

**Path:** `~/.config/u_crawler/config.toml`. See `assets/config.toml` for a
commented template — keep the two in step.

```toml
download_root = "~/Documents/Canvas-Backup"
concurrency = 4              # simultaneous in-flight requests
max_rps = 2                  # request pacing ceiling
user_agent = ""              # blank uses the built-in default

[logging]
level = "info"
file = "~/.config/u_crawler/u_crawler.log"

[canvas]
base_url = "https://<tenant>.instructure.com"
token = ""                   # optional if token_cmd is set
token_cmd = ""               # e.g. "pass show canvas/pat"
ignored_courses = []         # course ids to skip
sso_email = ""               # institutional SSO, used by the Zoom flow
sso_password = ""            # SECURITY: stored in cleartext

[announcements]
enabled = true
download_media = true

[calendar]
enabled = true
caldir_root = "~/Documents/Caldir"   # root of the caldir tree u_crawler owns exclusively

[layout]                     # layout v2 naming, see docs/specs/layout-v2.md
course_dir = "{code}-{name}" # placeholders {code}, {name}, {id}
code_pattern = ""            # regex on course_code; group 1 fills {code}
name_strip_pattern = ""      # regex removed from the name before {name}
section_pattern = ""         # regex for the section/NRC, used for -s<nrc> when a dir name is taken
term_pattern = ""            # regex for the academic term; group 1 fills curso.json's term

[zoom]
enabled = true
ffmpeg_path = "ffmpeg"
user_agent = "Mozilla/5.0 ..."
external_tool_id = 187       # the Canvas external tool id for Zoom
```

**Every key in this file is read by the code.** If you add one, wire it up in
the same commit; if you stop reading one, delete it. Inert configuration that is
documented as working is worse than no configuration.

---

## Directory Layout (Downloads)

Layout v2 (`docs/specs/layout-v2.md`). Every name comes from `layout.rs`;
writers never format a path themselves.

```
<download_root>/
  INDEX.md                           # courses + upcoming deadlines (full sync runs only)
  AGENTS.md                          # static: how to navigate the tree
  <ramo>/                            # course dir, see "Course directories" below
    README.md                        # modules, assignments, announcements, recordings
    curso.json                       # canvas_id, course_code, name, canvas_name, term, section, zoom_ids
    .u_crawler/state.json            # per-course sync state (file:, page:, assignment:,
                                     #   announcement:, calendar:, calendar-window:, zoom:)
    modulos/
      NN-<module-slug>/              # NN = Canvas module position
        README.md                    # links the module's pages and assignments
        NN-<page-slug>.md            # NN = item position; frontmatter tipo: page
        adjuntos/<attachment-slug>.<ext>
    tareas/<assignment-slug>.md      # frontmatter tipo: tarea (vence, disponible)
    anuncios/
      <YYYY-MM-DD>_<slug>.md         # frontmatter tipo: anuncio
      index.json                     # links, media and zoom links per announcement
      adjuntos/<attachment files>
    grabaciones/
      <YYYY-MM-DD>_<topic>[-parte-N].mp4
```

Every generated note goes through `migrate::rewrite::apply`: YAML
frontmatter (`ramo` = the course directory name) and Canvas `/files/<id>`
links rewritten to the local copy, so a sync writes exactly what the
migrator would. Files are downloaded before the note that links them is
written; a file that failed keeps its Canvas link.

**Course directories** (`library.rs`). A course owns the directory whose
`curso.json` names its Canvas id (or, with `canvas_id: null`, its name and
code). Newcomers are named by `layout::CourseNaming::course_dirs` over the
whole active course list sorted by id — before `--course-id` or
`ignored_courses` filtering, so a filtered run names a course the same way a
full one does. Any other directory at the root is reserved and never given
to a course, except one holding `.u_crawler/state.json` without `curso.json`
(an interrupted run, which its course resumes). Whoever creates a course
directory (`sync`, `announcements`, `calendar`, `zoom flow`) writes a minimal
`curso.json` at once; the end of `sync` rewrites it in full, keeping the
`zoom_ids` a migration recorded, and `term` / `section` when
`term_pattern` / `section_pattern` give none. In `curso.json`, `name` is the
readable name (`CourseNaming::display_name`: `name_strip_pattern` applied,
then `layout::humanize` — an all-caps `MINERIA_DE_DATOS` reads `Mineria de
Datos`, a name with lowercase letters keeps its case and accents) and
`canvas_name` the name as Canvas gives it (or as a v1 directory spelled it,
offline), which is what identifies a course without `canvas_id`. A
`curso.json` from before `canvas_name` has only `name`, read as that
identity. `README.md`, `INDEX.md` show the readable name.

**Name collisions inside a course.** Two assignments with the same title,
or two announcements with the same date and title: `layout::unique_names`,
in Canvas id order — each keeps the plain name unless a lower id was given
it, else gets the first free `-2`, `-3`… (`tarea-2.md`); a name depends only
on lower ids, so a new item never renames an existing one (a later
`Tarea 2` becomes `tarea-2-2.md`); the suffix survives slug truncation. The migrator numbers two
v1 notes the same way (announcements by the id in their v1 name,
assignments by their `NN` prefix), so a migrated note and the one `sync`
writes next agree. Attachments that slug to the same path in one
directory (`Guía 1.pdf`, `guia_1.PDF`) are settled by `download::Claims`:
each file keeps the name recorded in its `file:<id>` entry
(`ItemState::file_name`); a new one takes the plain name only when it is
free this run and nothing is on disk there, else `<slug>-<id>.<ext>`, a
name only that id ever writes. No file already named is moved or
overwritten by another. An announcement without `posted_at` is dated
`1970-01-01`, so its name is stable.

**Recordings** (`zoom::headless::plan_recordings`, pure). Named
`layout::recording_file(date, topic, part, course name)`; the date is the
meeting's `start_time`, else the segment's `recording_start`. Segments of one
meeting that would share a name are ordered by segment start and become
`-parte-2`, `-parte-3`…; an undated recording keeps v1's `zoom-<meeting id>`
fallback through `layout::attachment_file`. A recording is skipped when
`state.json` has its `zoom:<file id>` key without an error, **or** when a
file with its computed name is already in `grabaciones/` (the migrator
cannot know recording ids offline); the second case is recorded under its
key from then on. The file id is Zoom's `id` when the listing has one, else
`<meeting id>:<recording start>` (else `<meeting id>:<play url>`).

`INDEX.md` is rewritten only by a `sync` without `--course-id` (a filtered
run has not seen the other courses' deadlines); `AGENTS.md`, the READMEs and
`curso.json` are rewritten only when their text changed. A root `AGENTS.md`
or `INDEX.md` whose first line is not ours (the vault's own) is never
overwritten (`library::write_root_file`). `.u_crawler/library.json`
(`{"layout_version": 2}`) is written by the migration only, as its last step
(see "Migration").

### Directory Layout (Calendar)

```
<calendar.caldir_root>/
  <ramo>/
    deadlines/
      assignment-<assignment_id>.ics    # one VTODO per assignment with a due date
    windows/
      assignment-<assignment_id>.ics    # one VEVENT per assignment with unlock_at < due_at
```

Separate tree from `download_root`, and named per course the same way
(`library::course_dirs`, so a Canvas rename no longer moves it). A course
whose v1 directory (`fsutil::course_dir`) is still under `caldir_root` —
the library migration renames it along with the course, but a
calendar-only deployment has no v1 library — has it renamed by `calendar`
itself (`calendar::adopt_v1_caldir`, rename without replace) before its
UIDs are written; `--dry-run` reports the rename and plans against the v1
directory. When the v2 directory exists too, the course is skipped as
failed rather than write its UIDs twice. When `download_root` overlaps
`caldir_root` (the example container config), `calendar` names courses
from Canvas alone (an empty `library::Library`: the caldir tree is not a
library, and read as one its course directories would all be reserved),
writes no `curso.json` there, and its state lives at
`<caldir_root>/<ramo>/.u_crawler/state.json` — a v1 `state.json` carried
along by the rename is moved there. `deadlines/` and `windows/` are sibling directories
per course × semantics (spec D4), so a client can subscribe to one without
the other; a third directory for recurring-class semantics has room to land
later without moving anything under either. Filenames and UIDs derive from
the Canvas assignment id, never from any date, so a moved deadline or window
rewrites the same file in place. The `VEVENT` UID
(`u_crawler-window-{assignment_id}@u-crawler.local`) is deliberately distinct
from the `VTODO` UID (`u_crawler-todo-{assignment_id}@u-crawler.local`) —
UIDs are the CalDAV server-side identity and must be globally unique; see
Radicale issue #101 in `docs/specs/calendar-sync-flow.md` for the bug shape a
collision would reproduce. An assignment with no `due_at` produces neither
component. An assignment with `due_at` but no `unlock_at`, or with
`unlock_at` at or after `due_at` (inconsistent Canvas data), gets its `VTODO`
— without a `DTSTART`, though its description still reports the real
`unlock_at` Canvas gave — and no `VEVENT`. State that gates whether a
component is rewritten lives
under two distinct `state.json` namespaces — `calendar:{assignment_id}` for
the `VTODO`, `calendar-window:{assignment_id}` for the `VEVENT` — so writing
one component never marks the other "unchanged".

The deadline `VTODO` carries `SUMMARY` = `<Course.name> - <assignment name>`
(the human course name from Canvas, never the sanitized directory or the
course code; the two halves are joined with `" - "` only when both are
non-empty, so a nameless course or assignment never leaves a dangling dash),
a three-line `DESCRIPTION` (the same label, then
`Disponible: <unlock_at | "sin fecha de apertura"> - Vence: <due_at>` with
RFC 3339 UTC timestamps, then the assignment `html_url` — that last line is
dropped entirely when Canvas gives no URL), `DTSTART` = `unlock_at` **only
when `unlock_at` is strictly before `due_at`** (RFC 5545 §3.8.2.3 wants `DUE`
strictly later, so equal timestamps are excluded too), and `DUE` = `due_at`,
plus `URL`, `PRIORITY` and `STATUS`.

**The link is in `DESCRIPTION` on purpose even though `URL` already carries
it, and the due time is spelled out in the text even though `DUE` already
carries it. Neither is duplication to clean up.** Downstream, `caldir` never
sends `URL` to Google Tasks, and `DESCRIPTION` is the only free text that
arrives (as the task's `notes`); `DUE` reaches Google as a date only, computed
in the syncing machine's local zone, so the time of day survives nowhere else.
A separate start date and a deadline are **not writable** through the public
Google Tasks API — one writable date field, `due`, date-only, documented as the
*scheduled* date. That is a verified limitation of the destination, not a bug
here; `docs/specs/calendar-sync-flow.md` (D13 and "Límite verificado: Google
Tasks") carries the field matrix and the sources. Do not add fields, X-
properties or configuration trying to work around it, and do not reach for an
undocumented Google API.

Keep the `DESCRIPTION` short, plain and stable — no HTML (in particular not
`assignment.description`), no carriage returns, no trailing spaces. It is a
bidirectional field downstream and feeds the sync bridge's shared signature; a
blob Google might normalize turns into a sticky conflict that blocks
publication.

Text values on the `VTODO` path have `\r\n` and lone `\r` normalized to `\n`
before RFC 5545 §3.3.11 escaping — a surviving lone CR is read as a line
terminator downstream and silently splits the property in two — and every line
is folded to 75 octets per §3.1, always on a character boundary. Both are
`VTODO`-only, and live in that path rather than in `escape_text`, which
`render_vevent` shares: the `windows` `VEVENT` is neither normalized nor
folded, keeps `SUMMARY` = the bare assignment title, and renders byte-for-byte
as before.

An assignment recorded in `state.json` but absent from Canvas's response is
removed: both components' files are deleted and both state entries dropped, so
the calendar does not accumulate tasks the teacher has withdrawn. u_crawler
deletes only the local file — `caldir push` propagates the removal to the
server, which is why nothing here speaks CalDAV (spec D8).

**A course whose fetch failed is never reconciled.** `plan_for_course` takes
each fetch as a `Result` and returns `None` unless both succeeded, so a failed
course cannot reach the planner with an empty assignment list that would read
as "everything was deleted". This is structural rather than a convention to
remember: one network blip must not silently wipe a course's calendar under
cron, where nobody would notice until the data was gone.

---

## Migration

Layout v1 → v2 (`docs/specs/layout-v2.md`, "Reglas del migrador" and "API
de `migrate::ensure`"). Rename only: there is no file delete, copy or
overwrite anywhere in it. The one removal is `rmdir(2)` of the v1
directories the moves emptied (`Plan::prune`: v1 course directories — or their `Modules/` and
`announcements/` when the v2 name is the same —
mapped `Zoom/<id>`, then `Zoom/` if empty), bottom-up, each journaled as
`rmdir` first; never `remove_dir_all`. A directory with anything left in it
(a duplicate, a foreign file, a symlink) stays and is reported. `--undo`
creates the removed directories again and removes the ones the run created
once they are empty.

Two v1 notes that are different Canvas items and want one v2 name (two
announcements whose v1 names carry different ids, two `NN-ASSIGN-` files
with one title and different `NN`) are numbered `-2`, `-3`… by
`layout::unique_names` over every item of the directory in id / `NN`
order, never treated as a collision. Duplicate/conflict
handling is only for sources of the same item (same id / `NN`).

`migrate::ensure::migrate` (the `migrate` command) runs, in this order:

1. `apply::pending` → `apply::resume`: a run cut short is finished from its
   journal's intent before anything is planned.
2. `scan` → `plan` → `apply` under a new journal
   `.u_crawler/migrations/<ts>.jsonl`, when the plan is not empty. With a
   `CanvasClient` (`--canvas`, and always from the writer commands), Canvas
   supplies official names and module positions (`ensure::canvas_courses`:
   the active list plus `GET /courses/:id` for ids an `index.json` gave);
   offline, modules are numbered in id order.
3. Every generated note a journal renamed is rewritten
   (`rewrite::rewrite_file`, backup under `.u_crawler/backup/<ts>/`, the
   journal's `<ts>`): `ramo` + `tipo`, and for an announcement `canvas_id`
   (from the v1 file name), `url` and `publicado` (from `index.json`), plus
   Canvas `/files/<id>` links to the announcement media `index.json` lists.
   Pages and assignments keep their other keys until the next `sync`. The
   notes come from the journals, not the plan: without `library.json`, every
   finished journal is replayed (idempotent), so a cut between the renames
   and the rewrites is healed by the next run; with it, only this call's.
   A note that fails is logged and counted, never fatal.
4. Every index file, from the disk (`library::write_indexes_from_disk`,
   always, offline or not): a `README.md` per course and per module where
   there is none — one already there (a `sync`'s, or the user's) is never
   overwritten — each `curso.json` brought up to date (readable `name`,
   `canvas_name`, `term` and `section` when it had none), and root
   `AGENTS.md` and `INDEX.md`, with the deadlines the assignments'
   frontmatter records. From disk, modules are named by the first line of
   an existing module README, else by their slug; page, assignment and
   recording titles come from file names (`layout::humanize_slug`),
   announcement titles from `index.json`; a module README lists its pages
   only (which module links which assignment is not on disk; `sync` adds
   it).
5. `.u_crawler/library.json`, last: until it exists the library is not v2.

`ensure_v2` is the cheap gate the writer commands use: a root that does not
exist is left alone; `library.json` present and no open journal returns at
once without scanning or calling Canvas — unless a course or module lacks
its `README.md` (`library::indexes_missing`, directory listings only), when
step 4 runs (not in a dry run); otherwise it is `migrate`. A
library root that overlaps `calendar.caldir_root` is never treated as a
library (`MigrateError::Overlap` from `migrate`). `migrate` ignores the
marker and always plans, so a v1 course copied into a v2 library is migrated
by running it explicitly.

`migrate --root P` migrates `P` and passes no caldir root (unless `P` is
`download_root` itself): that is how a `cp -al` clone is migrated without
touching the real calendar tree. Clone
only when no journal is open: a resumed journal is appended to, and in a
hardlink clone it is the original's inode.

`migrate --undo <ts>` (`ensure::undo`): refuses an unknown journal or one
with a newer journal still in effect, before touching anything; puts each
rewritten note back from its backup, setting the current bytes aside under
`.u_crawler/migrations/<ts>.undo/<path>`; then `apply::undo`; then the
`README.md` files of every course directory the journal created (its
`curso.json` set aside), which would otherwise make the emptied directory
a foreign one the next migration avoids; then removes the directories the
run created that this left empty (`apply::remove_created_dirs`); then sets
`library.json` aside the same way, so the next writer command migrates
again (for example after a `[layout]` change). Root `AGENTS.md`/`INDEX.md`
stay. Every step is idempotent, so a cut undo is simply run again.

## Architecture

```
src/
  main.rs           # CLI parsing, command dispatch, all user-facing printing
  lib.rs            # module docs and the layer map
  config.rs         # load/validate/expand config
  http.rs           # the one client: rate limiting, 429/5xx retry, Link parsing
  canvas.rs         # Canvas REST client (get_json / list_paginated)
  download.rs       # the one downloader: ETag, .part, Range resume, atomic rename
  state.rs          # per-course State, ItemState, record_error
  links.rs          # HTML -> links, media refs, zoom URLs; Canvas /files/<id> markdown rewrite
  syncer.rs         # CourseSync / ModuleCtx: the main sync flow
  announcements.rs  # AnnouncementSync
  calendar.rs       # calendar-sync: pure deadline planner (`plan`) + its I/O executor (`run_calendar`)
  recordings.rs     # zoom-link discovery report
  fsutil.rs         # sanitization, atomic write/rename
  frontmatter.rs    # YAML frontmatter for generated notes (render, parse, upsert)
  layout.rs         # layout v2: pure metadata -> name functions (compiled from [layout] at config load)
  library.rs        # which directory each course owns (curso.json), shared by every writer; index-file writes
  index.rs          # layout v2 indexes: pure rendering of course/module README, curso.json, INDEX.md and AGENTS.md
  migrate/
    mod.rs          # read-only scan into an Inventory (files+sizes, index.json ids, curso.json, caldir dirs)
    detect.rs       # pure: v1 / v2 / empty, from the inventory
    plan.rs         # pure: inventory (+ optional Canvas metadata) -> Plan { courses, moves, edits, duplicates, conflicts, unresolved }
    apply.rs        # executor: write-ahead journal (.u_crawler/migrations/<ts>.jsonl), rename without replace, BLAKE3 collisions, resume, undo
    rewrite.rs      # backup under .u_crawler/backup/<ts>/, then atomic rewrite of generated markdown
    ensure.rs       # the whole migration: resume, plan, apply, rewrite, root indexes, library.json; ensure_v2; undo
  ffmpeg.rs         # ffmpeg invocation
  progress.rs       # progress bars, registered with ui::bars()
  ui.rs             # user-facing output channel (status! macro)
  logger.rs         # log file setup, falls back to stderr
  zoom/
    mod.rs          # zoom_flow orchestration
    api.rs          # Zoom recordings REST client
    db.rs           # SQLite session store, ZoomSession, ZoomDbError
    models.rs       # wire types
    headless.rs     # browser driving and CDP interception
    sso.rs          # Canvas -> Microsoft -> Zoom login, free functions over a Page
    app_conf.rs     # pure parser for Zoom's window.appConf blob
    download.rs     # HTTP fallback when ffmpeg cannot fetch a recording
```

Rules that hold today and should keep holding:

* **All HTTP goes through `HttpCtx`.** It is the only place rate limiting and
  retries live. A raw `reqwest::Client::send()` outside `http.rs` is a bug.
* **Every writer gets its course directory from `library::course_dirs`** and
  its names from `layout.rs`. Nobody formats a library path by hand.
* **All Canvas file downloads go through `download::download_if_needed`,** under
  one state-key namespace (`file:{id}`), so a file reachable from both a module
  and an announcement is fetched once.
* **All Canvas list endpoints go through `CanvasClient::list_paginated`.**
* **`zoom::sso` must not touch the database or the course id.** It takes a page
  and credentials. That is what makes it testable.
* **`migrate::rewrite` rewrites only markdown `generated_kind` accepts.** It
  copies the original bytes to `<root>/.u_crawler/backup/<ts>/` and then
  replaces the note with `fsutil::atomic_write` (a new inode, so a `cp -al`
  clone keeps the old one). A note that already matches is left alone and gets
  no backup. A leading `---` is frontmatter only when the interior is a
  property list; html2md's `<hr>` (a blank line after the fence) and an
  indented prose line between two rules stay in the body. An existing
  non-symlink `.part` beside the note or the backup is refused (`PartExists`);
  `atomic_write` itself is unchanged and downloads still truncate their own
  `.part`. `migrate::ensure` is its only caller outside the writers. The frozen signatures are in
  `docs/specs/layout-v2.md` ("API de frontmatter y de la reescritura").
* Structured log fields: `course_id`, `module_id`, `file_id`, `path`, `attempt`.

---

## Canvas API Contract (v1)

* Courses: `GET /api/v1/courses?enrollment_state=active&per_page=100`
* Modules (+items): `GET /api/v1/courses/{id}/modules?include=items&per_page=100`
* Assignments: `GET /api/v1/courses/{id}/assignments?per_page=100`
* Submissions (own, bulk per course): `GET /api/v1/courses/{id}/students/submissions?student_ids[]=self&per_page=100`
* Announcements: `GET /api/v1/courses/{id}/discussion_topics?only_announcements=true&per_page=100`
* Pages: `GET /api/v1/courses/{id}/pages/{slug}`
* Course by id (migration only, answers for inactive courses): `GET /api/v1/courses/{id}`
* Files: `GET /api/v1/files/{id}`; download via `download_url` or `url`
* Pagination: the `Link` header, `rel="next"`, capped at `MAX_PAGES`
* Backoff: honour `Retry-After` (capped), exponential for 5xx, bounded by
  `max_retries`

---

## Zoom Recording Flow

1. `ZoomDb::load_session` looks for a complete stored session (scid, cookies,
   and the `x-xsrf-token` / `x-zm-*` headers). Partial is treated as absent.
2. If absent or rejected, `headless.rs` launches a browser, navigates to the
   Canvas external tool, and lets `sso.rs` drive the Canvas → Microsoft login.
   CDP Fetch interception scrapes `window.appConf` (`app_conf.rs`) for the
   identifiers, and the cookies are harvested and stored.
3. `api.rs` lists meetings and their recording files.
4. `headless.rs` visits each play URL, captures the short-lived asset request
   headers, and downloads via `ffmpeg` (stream copy), falling back to a plain
   HTTP download when ffmpeg fails.

`ffmpeg` must be present; `zoom.ffmpeg_path` points at it.

---

## Running CI locally

The repo ships an `.actrc`, so [`act`](https://github.com/nektos/act) reproduces
the GitHub Actions jobs in a container:

```bash
act pull_request -l                                        # list jobs
act pull_request -j clippy -s GITHUB_TOKEN="$(gh auth token)"
```

`check`, `clippy` and `test` each run as a 2-entry matrix (`default` and
`no-default`) covering both feature configurations from "Building without
Zoom" above; `build-check` adds a `no-default-features` entry for the musl
target specifically, since that combination is the one the spec calls out as
fragile and the one that matters for the cron image. `act` runs one matrix
entry at a time — pass `--matrix features:no-default` (or `features:default`)
to pick one.

Pass the token explicitly — `act` needs it to clone the actions themselves, and
the checked-in `.secrets` file is not guaranteed to hold a current one. The
`build-check` matrix includes a Windows target that `act` cannot run locally;
Linux jobs are the ones worth reproducing.

**Two things `act` does not catch**, both of which have bitten this branch:

1. **It bind-mounts the working tree instead of checking out a commit**, so it
   tests your branch — not the `refs/pull/N/merge` commit that CI actually
   builds. When `main` moves, git auto-merges `Cargo.lock` into an incoherent
   hybrid that `--locked` rightly rejects, and `act` sees none of it. Merge
   `origin/main` and re-verify before trusting a green local run.
2. **It reuses your `CARGO_HOME`**, where every crate is already cached, so a
   lockfile that needs re-resolution can pass locally and fail on a clean
   runner.

To reproduce CI properly, use a clean checkout and a clean cargo home:

```bash
git clone --depth 1 -b <branch> <url> /tmp/cichk
docker run --rm -v /tmp/cichk:/src -w /src rust:latest \
  bash -c 'export CARGO_HOME=/tmp/ch; cargo check --locked'
```

`release.yml`, `changelog.yml` and `container.yml` must **not** be run with
`act` as they stand: they push commits, create GitHub releases, and push an
image to GHCR. Review them statically — `actionlint <file>` catches the
mechanical errors.

`container.yml`'s tag logic is worth exercising, though, since the rules differ
per event. Copy it to a scratch file, delete the login step and truncate it
after the `metadata` step (so no credential and no push survive), echo
`steps.meta.outputs.tags`, and run that copy with `-e` event payloads for
`refs/tags/v1.2.3`, a `-beta.1` pre-release, and a branch dispatch. The three
cases that matter: a release tag gets `latest`, a pre-release does not, and a
`workflow_dispatch` on a branch still yields a non-empty tag list — the semver
rules alone emit nothing off a tag, which is what `type=ref,event=branch` and
`type=sha` are there to cover.

`cliff.toml` can be validated on its own, which has no side effects:

```bash
docker run --rm -v "$PWD":/app -w /app \
  ghcr.io/orhun/git-cliff/git-cliff:latest --config cliff.toml --latest
```

---

## Known rough edges

Documented so they are not rediscovered as surprises:

* **`sync` aborts on a page fetch failure.** `CourseSync::sync_module` uses `?`
  on `canvas.get_page`, and `run_sync` uses `?` on `sync_module`. A single
  unreadable page therefore kills the whole run, including courses not yet
  reached — while an unreadable *file* is merely recorded via
  `State::record_error` and skipped. The file behaviour is the intended one;
  aligning pages and assignments with it is the obvious fix. `calendar` does
  **not** share this edge: a course whose assignment fetch fails is logged and
  skipped, the rest of the run continues, and the run reports exit code `13`
  if the failure was partial (spec ticket 11).
* **The `URL` value is escaped as if it were `TEXT`.** `calendar.rs` runs
  `escape_text` over the `html_url` for both components, but RFC 5545 §3.8.4.6
  types `URL` as `URI`, which takes no backslash escaping — so a Canvas URL
  containing a comma would be emitted wrong. No observed Canvas URL has one,
  `caldir` preserves the value verbatim and never sends it to Google anyway,
  and the function is shared with the frozen `windows` `VEVENT`. Known debt,
  deliberately unfixed; fixing it means touching `VEVENT` bytes.
* **Two writer commands started at once on an unmigrated library both
  migrate.** `ensure_v2` takes no lock. Renames never replace and each run
  has its own journal, so nothing is lost, but the slower run can stop with
  `SourceMissing`; running it again finishes cleanly. Start the first run of
  a new version alone (or run `u_crawler migrate` first).
* **`recordings` does not honour `canvas.ignored_courses`,** while `sync` and
  `announcements` both do.
* **`scan --course-id` counts file items but does not list them,** unlike the
  course listing which prints each entry.

---

## Testing

Present today (`cargo test`): Link-header parsing, retry/backoff classification,
URL construction and percent-encoding, `window.appConf` parsing, link/media
extraction, filename sanitization, `State::record_error`, page-slug extraction,
byte-size formatting, char-safe truncation, Zoom cookie expiry, YAML frontmatter
round-trip and upsert, Canvas `/files/<id>` markdown rewrite, course
directory resolution and index-file writes (`library`), assignment and
announcement naming with collisions, recording naming / parts / `zoom:`
dedupe (`zoom::headless`), the `zoom:` state key,
`migrate::rewrite` (generated-path gate, backup, resume, a leading horizontal
rule left in the body, refusal of an existing `.part`, and on Unix the
hardlink inode), layout v2 index rendering, the synthetic layout-v1
library (`tests/fixtures/v1/`, written under a temp dir; no real course data —
see `docs/specs/layout-v2.md`), the migration plan over that library
(`tests/migrate_plan.rs`: golden snapshot in `tests/golden/migrate_plan_v1.txt`,
regenerated with `U_CRAWLER_BLESS=1`; no-loss and second-run-is-empty checks),
and its execution (`tests/migrate_apply.rs`: a cut at several points resumes
from the journal to the same tree and directories, undo restores the
manifest byte for byte and the same set of directories, no empty v1
directory is left after a migration while one holding a file is kept and
reported, two distinct items with one name are numbered rather than
collided, a second run is a no-op, identical/different collisions, and a `cp -al` clone
whose markdown is rewritten leaves the original untouched), the index files
an offline migration writes (`tests/migrate_indexes.rs`: golden snapshot of
every `README.md`, `curso.json` and `INDEX.md` in
`tests/golden/migrate_indexes_v1.txt`, regenerated with `U_CRAWLER_BLESS=1`;
no relative link points at a missing file; readable name, term and section;
`ensure_v2` writes back missing READMEs and leaves an existing one alone), and the whole
migration end to end, offline, as a `sync` runs it (`tests/migrate_ensure.rs`:
it migrates and marks the library, a second pass does nothing, every hash of
the v1 manifest survives (rewritten notes in their backup), the calendar
directory follows its course, a dry run touches nothing, a cut mid-apply or
after the renames heals on the next run, undo restores the v1 tree byte for
byte, and a caldir used as `download_root` is never marked).

Worth adding: a mock-server integration test covering pagination across three
pages, ETag skip, and `Range` resume. `tests/` covers the Zoom database and
that v1 fixture.

---

## Security

* Never print or log tokens, cookies, the `lti_scid`, or the SSO password.
* `sso_password` sits in cleartext in `config.toml`. The file is written
  atomically and chmod'd `0600` on Unix (`config::save_config_to_path`), but
  cleartext is still cleartext — an `sso_password_cmd` mirroring `token_cmd`
  would be the right fix.
* Retrieve the Canvas PAT via `token_cmd` (`pass`, `gopass`) where possible.
* Do not commit captured API responses; they contain real names and addresses.

---

## Agent skills

Per-repo configuration for the `mattpocock/skills` engineering skills
(`research`, `to-spec`, `to-tickets`, `implement`, `tdd`, `triage`,
`code-review`). Written by `setup-matt-pocock-skills`; edit the files under
`docs/agents/` directly rather than re-running it.

### Issue tracker

GitHub Issues on `belcaik/unab-sync-content`, via the `gh` CLI, falling back to
local markdown tickets under `.scratch/<feature>/issues/` when `gh` is not
authenticated. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical role names, unchanged: `needs-triage`, `needs-info`,
`ready-for-agent`, `ready-for-human`, `wontfix`. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and one `docs/adr/` at the repo root, created
lazily. See `docs/agents/domain.md`.

---

## Agent Operating Instructions

* Prefer deleting complexity to rearranging it.
* Write tests alongside code, especially for anything pure.
* Keep public APIs documented (`///`) and modules documented (`//!`).
* No `unwrap`/`expect` outside tests, except where failure is genuinely
  impossible (a `Regex::new` on a literal in a `LazyLock`) — and say so.
* No unsafe code without justification.
