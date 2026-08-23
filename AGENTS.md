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
| `calendar` | `--course-id`, `--dry-run` | Project assignment deadlines to `.ics` `VTODO` files under `calendar.caldir_root`. |
| `recordings` | `--course-id`, `--dry-run` | Report Zoom links found across a course. Does not download. |
| `zoom flow` | `--course-id`, `--since` | Capture a Zoom session and download its recordings. |
| `status` | `--verbose` | Per-course file counts, storage, last sync, failed items. |

`sync` honours `[announcements].enabled` and `[zoom].enabled`; either can be
turned off without touching the command line.

### Exit Codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `10` | Config error, including "config was just created, go edit it" |
| `11` | Auth error |
| `12` | Network / rate-limit / runtime failure |
| `13` | `calendar` partial failure: at least one course synced and at least one failed. All courses failing is **not** this code — it is a hard failure and surfaces as `12` instead. |

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

```
<download_root>/
  <Course Name>_<Course Code>/
    state.json                       # per-course sync state
    Modules/
      <module_id>_<Module Name>/
        NN-<Page Title>.md           # page body as markdown
        NN-ASSIGN-<Title>.md         # assignment description
        Attachments/
          <original-filename>
    announcements/
      <YYYY-MM-DD>_<slug>_<id>.md
      index.json                     # links, media and zoom links per announcement
      media/
        <attachment files>

<download_root>/Zoom/<course_id>/
  <recording>.mp4
```

There is no week-folding. Names are sanitized and transliterated to ASCII by
`fsutil::sanitize_component`.

### Directory Layout (Calendar)

```
<calendar.caldir_root>/
  <Course Name>_<Course Code>/
    deadlines/
      assignment-<assignment_id>.ics    # one VTODO per assignment with a due date
```

Separate tree from `download_root`, and named per course the same way
(`fsutil::course_dir`). A sibling directory for availability-window `VEVENT`s
(spec D4) is planned for a later ticket and will not require moving anything
under `deadlines/`. Filenames and UIDs derive from the Canvas assignment id,
never from the due date, so a moved deadline rewrites the same file in place.

---

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
  links.rs          # HTML -> links, media refs, zoom URLs
  syncer.rs         # CourseSync / ModuleCtx: the main sync flow
  announcements.rs  # AnnouncementSync
  calendar.rs       # calendar-sync: pure deadline planner (`plan`) + its I/O executor (`run_calendar`)
  recordings.rs     # zoom-link discovery report
  fsutil.rs         # sanitization, atomic write/rename
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
* **All Canvas file downloads go through `download::download_if_needed`,** under
  one state-key namespace (`file:{id}`), so a file reachable from both a module
  and an announcement is fetched once.
* **All Canvas list endpoints go through `CanvasClient::list_paginated`.**
* **`zoom::sso` must not touch the database or the course id.** It takes a page
  and credentials. That is what makes it testable.
* Structured log fields: `course_id`, `module_id`, `file_id`, `path`, `attempt`.

---

## Canvas API Contract (v1)

* Courses: `GET /api/v1/courses?enrollment_state=active&per_page=100`
* Modules (+items): `GET /api/v1/courses/{id}/modules?include=items&per_page=100`
* Assignments: `GET /api/v1/courses/{id}/assignments?per_page=100`
* Announcements: `GET /api/v1/courses/{id}/discussion_topics?only_announcements=true&per_page=100`
* Pages: `GET /api/v1/courses/{id}/pages/{slug}`
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

`release.yml` and `changelog.yml` must **not** be run with `act`: they push
commits and create GitHub releases. Review them statically. `cliff.toml` can be
validated on its own, which has no side effects:

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
* **`recordings` does not honour `canvas.ignored_courses`,** while `sync` and
  `announcements` both do.
* **`scan --course-id` counts file items but does not list them,** unlike the
  course listing which prints each entry.

---

## Testing

Present today (`cargo test`): Link-header parsing, retry/backoff classification,
URL construction and percent-encoding, `window.appConf` parsing, link/media
extraction, filename sanitization, `State::record_error`, page-slug extraction,
byte-size formatting, char-safe truncation, and Zoom cookie expiry.

Worth adding: a mock-server integration test covering pagination across three
pages, ETag skip, and `Range` resume — `tests/` currently only covers the Zoom
database.

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

## Agent Operating Instructions

* Prefer deleting complexity to rearranging it.
* Write tests alongside code, especially for anything pure.
* Keep public APIs documented (`///`) and modules documented (`//!`).
* No `unwrap`/`expect` outside tests, except where failure is genuinely
  impossible (a `Regex::new` on a literal in a `LazyLock`) — and say so.
* No unsafe code without justification.
