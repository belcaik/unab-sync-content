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

---

## Tech Stack

* **Language:** Rust (Edition 2021)
* **Async runtime:** `tokio`
* **HTTP:** `reqwest` (gzip/brotli/deflate, streaming, rustls)
* **CLI:** `clap` (derive)
* **Config:** TOML (`directories`, `toml`)
* **Parsing:** `serde`, `serde_json`, `regex`, `url`, `html2md`
* **Storage:** `rusqlite` (Zoom session store), JSON (`state.json` per course)
* **Browser automation:** `chromiumoxide` (pinned to a git rev — see Cargo.toml)
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
* **No `#[allow(clippy::...)]`** without a comment justifying it. There are
  currently none in the crate; adding one is a design signal, not a fix.
* **Tests:** `cargo test`. Prefer pure functions that can be tested without a
  network or a browser — that is why `links`, `download`, `zoom::app_conf` and
  `zoom::sso` are separate from the flows that call them.

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

Codes 13–15 are not currently emitted. Do not document them until they are.

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

## Known rough edges

Documented so they are not rediscovered as surprises:

* **`sync` aborts on a page fetch failure.** `CourseSync::sync_module` uses `?`
  on `canvas.get_page`, and `run_sync` uses `?` on `sync_module`. A single
  unreadable page therefore kills the whole run, including courses not yet
  reached — while an unreadable *file* is merely recorded via
  `State::record_error` and skipped. The file behaviour is the intended one;
  aligning pages and assignments with it is the obvious fix.
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
