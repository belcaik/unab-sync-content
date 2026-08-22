# u_crawler

A command-line tool that mirrors your Canvas LMS courses — pages, assignments,
attachments, announcements — and the Zoom cloud recordings linked from them, to
a folder on your machine.

Syncs are incremental and resumable. Re-running is always safe: unchanged files
are skipped via their ETag, and interrupted downloads pick up where they left
off.

---

## Contents

- [How it works](#how-it-works)
- [Requirements](#requirements)
- [Install](#install)
- [Getting started](#getting-started)
- [Commands](#commands)
- [Configuration](#configuration)
- [What ends up on disk](#what-ends-up-on-disk)
- [Zoom recordings](#zoom-recordings)
- [Exit codes](#exit-codes)
- [Troubleshooting](#troubleshooting)
- [Development](#development)
- [License](#license)

---

## How it works

Canvas content is fetched with a Personal Access Token over the REST API. Pages
and assignment descriptions are converted from HTML to Markdown; files linked
from either are downloaded alongside them.

Zoom is different. Recordings are not exposed through a token-based API — they
sit behind an LTI launch from inside Canvas. So `u_crawler` launches a headless
Chrome, completes your institution's single sign-on, and captures the session
identifiers the recordings API needs. Those are cached in a local SQLite
database and reused until they stop being accepted.

Every request in the tool goes through one rate-limited, retrying HTTP client,
so `max_rps` and `concurrency` apply to everything.

---

## Requirements

| Requirement | Notes |
|---|---|
| Rust 1.70+ | Only to build from source. |
| Git | One dependency (`chromiumoxide`) is fetched from a Git repository at build time, so the build needs network and `git`. |
| ffmpeg | Required for Zoom recordings. Not needed if `zoom.enabled = false`. |
| Chrome or Chromium | Launched automatically for the Zoom login. You do not start it yourself. |

Linux and macOS are the primary targets; the release workflow also builds for
Windows.

---

## Install

```bash
git clone https://github.com/belcaik/unab-sync-content.git
cd unab-sync-content
cargo build --release
```

The binary lands at `target/release/u_crawler`. Put it on your `$PATH`, or run
it in place with `cargo run --release -- <command>`.

Verify:

```bash
u_crawler --version
u_crawler --help
```

---

## Getting started

**1. Create the config.**

```bash
u_crawler init
```

The first run of *any* command creates a default config and exits with code 10
so you notice you need to edit it. `init` makes that explicit.

**2. Get a Canvas token.** In Canvas: *Account → Settings → Approved
Integrations → + New Access Token*.

**3. Store it.**

```bash
u_crawler auth canvas \
  --base-url https://canvas.your-school.edu \
  --token 1234~abcdef...
```

Or keep it out of the config file entirely by storing a command that prints it:

```bash
u_crawler auth canvas \
  --base-url https://canvas.your-school.edu \
  --token-cmd "pass show canvas/pat"
```

`--token` and `--token-cmd` are mutually exclusive and one is required; setting
either clears the other. The config file is written with `0600` permissions on
Unix.

**4. Check the connection.**

```bash
u_crawler scan
```

This lists your active courses with their ids. Nothing is written.

**5. Preview, then sync.**

```bash
u_crawler sync --dry-run          # reports what would happen, writes nothing
u_crawler sync
```

Set `download_root` in the config before this — the default points at a path
that probably is not yours.

---

## Commands

### `init`

Creates the default config if it is missing, and reports the path. Leaves an
existing config untouched.

### `auth canvas`

Stores Canvas credentials in the config.

| Flag | Required | Description |
|---|---|---|
| `--base-url URL` | no | Your Canvas instance, e.g. `https://canvas.your-school.edu` |
| `--token TOKEN` | one of | The Personal Access Token itself |
| `--token-cmd CMD` | one of | A shell command that prints the token; run via `sh -lc`, output trimmed |

### `scan`

Read-only inspection. With no arguments, lists active courses as
`- [id] Name - CODE`. With `--course-id`, lists that course's modules and a
count of the file items in them.

| Flag | Description |
|---|---|
| `--course-id ID` | Inspect one course's modules instead of listing courses |

Files are enumerated through module items rather than the course files
endpoint, which many Canvas instances return 403 for.

### `sync`

The main command. For each course: module pages and assignment descriptions as
Markdown, every file they link to, then announcements, then Zoom recordings.

| Flag | Description |
|---|---|
| `--course-id ID` | Sync only this course |
| `--dry-run` | Report planned actions; write nothing, download nothing, launch no browser |
| `--verbose` | Also log items that were skipped as unchanged |

Courses listed in `canvas.ignored_courses` are skipped. The announcements and
Zoom stages honour `announcements.enabled` and `zoom.enabled`.

### `announcements`

Announcements only, without the rest of a sync. Each announcement body is saved
as Markdown, its links and media references are extracted, Canvas-hosted
attachments are downloaded, and an `index.json` records all of it.

| Flag | Description |
|---|---|
| `--course-id ID` | Only this course |
| `--dry-run` | Report counts; write nothing |

### `recordings`

A discovery report: scans course pages, module items and assignment
descriptions for Zoom URLs and prints where each one was found. **This command
downloads nothing** — use `zoom flow` for that.

| Flag | Description |
|---|---|
| `--course-id ID` | Only this course |
| `--dry-run` | Prefixes output lines with `DRY-RUN` |

### `zoom flow`

Captures a Zoom session and downloads that course's recordings. See
[Zoom recordings](#zoom-recordings).

| Flag | Required | Description |
|---|---|---|
| `--course-id ID` | yes | Target course |
| `--since DATE` | no | Only recordings after this date |

### `status`

Reads each course's `state.json` and reports tracked file count, storage used,
the most recent `updated_at` seen, and how many items failed on their last
attempt.

| Flag | Description |
|---|---|
| `--verbose` | List each failed item with its error and attempt count |

```
Backup Status:

Course: Calculo_II_MAT2200
  Files: 143
  Storage: 1.24 GB
  Last sync: 2026-03-01T12:00:00Z
  Failed downloads: 2 items need retry
      Run with --verbose to see details

─────────────────────────────
Total: 4 courses, 512 files, 6.80 GB
```

This reads `state.json` files only — it does not contact Canvas, so it is
instant and works offline.

---

## Configuration

The config file lives at:

| Platform | Path |
|---|---|
| Linux | `~/.config/u_crawler/config.toml` |
| macOS | `~/Library/Application Support/u_crawler/config.toml` |
| Windows | `%APPDATA%\u_crawler\config.toml` |

Every key below is read by the code. There are no inert settings.

```toml
# Where the backup tree is written. Required. `~` is expanded.
download_root = "~/Documents/Canvas-Backup"

# Simultaneous in-flight HTTP requests.
concurrency = 4

# Request pacing ceiling, in requests per second. 0 disables pacing.
max_rps = 2

# Leave empty to use the built-in default.
user_agent = ""

[logging]
# trace | debug | info | warn | error
level = "info"
# Appended to, never rotated. Falls back to stderr if it cannot be opened.
file = "~/.config/u_crawler/u_crawler.log"

[canvas]
base_url = "https://canvas.your-school.edu"

# Provide exactly one of these.
token = ""
token_cmd = "pass show canvas/pat"

# Course ids to skip, as strings.
ignored_courses = ["153095", "153607"]

# Institutional SSO, used only by the Zoom flow.
# SECURITY: sso_password is stored in cleartext. The file is written 0600 on
# Unix, but anything that can read your home directory can read it.
sso_email = "you@your-school.edu"
sso_password = ""

[announcements]
enabled = true          # include announcements in `sync`
download_media = true   # also download attachments and inline media

[zoom]
enabled = true          # false skips Zoom entirely during `sync`
ffmpeg_path = "ffmpeg"  # or an absolute path
user_agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 ..."
external_tool_id = 187  # the Zoom LTI tool id in your Canvas
```

### Validation

Loading fails, listing the offending keys, if:

- `download_root` is empty
- `canvas.base_url` is empty or still contains the `<tenant>` placeholder
- both `canvas.token` and `canvas.token_cmd` are empty
- `zoom.enabled` is true and `zoom.ffmpeg_path` is empty

### Finding `external_tool_id`

Open the Zoom tab inside a Canvas course in your browser. The URL contains
`/external_tools/<id>` — that number is the value. The default of `187` is
specific to one institution and is unlikely to be right for yours.

---

## What ends up on disk

```
<download_root>/
├── <Course Name>_<COURSE_CODE>/
│   ├── state.json                            sync state for this course
│   ├── Modules/
│   │   └── <module_id>_<Module Name>/
│   │       ├── 01-<Page Title>.md            page body as Markdown
│   │       ├── 02-ASSIGN-<Title>.md          assignment description
│   │       └── Attachments/
│   │           └── <original-filename.pdf>
│   └── announcements/
│       ├── <YYYY-MM-DD>_<title-slug>_<id>.md   date omitted if unknown
│       ├── index.json
│       └── media/
│           └── <attachments and inline media>
└── Zoom/
    └── <course_id>/
        └── <YYYY-MM-DD - Meeting Topic>.mp4
```

Directory and file names are sanitized and transliterated to ASCII, so
`Introducción` becomes `Introduccion`. Numeric prefixes (`01-`, `02-`) follow
the module's own item ordering. Announcement slugs are capped at 60 characters,
and the date prefix is dropped if Canvas reports no `posted_at`. Recording
names that would collide get a `_1`, `_2` suffix rather than overwriting.

### `state.json`

Maps an item key to what is known about it. Keys are namespaced:
`file:<id>`, `page:<slug>`, `assignment:<id>`, `announcement:<id>`.

```json
{
  "items": {
    "file:20411861": {
      "etag": "a1b2c3",
      "updated_at": "2026-03-01T12:00:00Z",
      "size": 184320,
      "content_hash": null,
      "last_error": null,
      "error_count": null
    }
  }
}
```

`etag` drives the skip-if-unchanged decision for files; `content_hash` does the
same for generated Markdown. `last_error` and `error_count` are what `status`
reports. A file linked from both a module and an announcement is one entry, and
is downloaded once.

### `announcements/index.json`

An array of records, one per announcement:

```json
[
  {
    "id": 123,
    "title": "Semana 3 - Taller",
    "posted_at": "2026-03-01T12:00:00Z",
    "html_url": "https://canvas.your-school.edu/courses/1/discussion_topics/123",
    "author": "Nombre Apellido",
    "body_md_path": "announcements/2026-03-01_Semana_3_Taller_123.md",
    "links": ["https://..."],
    "media": [
      {
        "url": "https://canvas.your-school.edu/courses/1/files/456",
        "kind": "canvas_file",
        "file_id": 456,
        "local_path": "announcements/media/taller.ipynb"
      }
    ],
    "zoom_links": ["https://x.zoom.us/rec/play/..."]
  }
]
```

`kind` is one of `image`, `video`, `audio`, `canvas_file`, `link`. Paths are
relative to the course directory. `local_path` is null when the media was not
downloaded — either because `download_media` is false, or because it is not
hosted on Canvas.

---

## Zoom recordings

```bash
u_crawler zoom flow --course-id 123456
u_crawler zoom flow --course-id 123456 --since 2026-01-01
```

Set `canvas.sso_email` and `canvas.sso_password` first — the flow logs in on
your behalf.

**What happens:**

1. A complete cached session is looked for in
   `<config_dir>/zoom_state.sqlite`. Partial credentials count as none.
2. If there is none, or Zoom rejects it, a headless Chrome opens the Zoom
   external tool in Canvas and completes the Canvas → Microsoft → Zoom login.
   The session identifiers are scraped from the page and stored.
3. Meetings and their recording files are listed through Zoom's API.
4. Each recording's play page is visited to capture its short-lived asset URL
   and headers, then downloaded immediately — those URLs expire quickly, which
   is why recordings are processed one at a time rather than in parallel.

Downloads use `ffmpeg` in stream-copy mode:

```
ffmpeg -y -loglevel error -hide_banner \
  -headers "<captured headers>" \
  -i "<asset url>" \
  -c copy -map 0 -movflags +faststart "<dest>.mp4"
```

If `ffmpeg` fails, a plain HTTP download is attempted as a fallback. Recordings
already present on disk are skipped without re-visiting them.

`sync` runs this same flow per course, unless `--dry-run` is set or
`zoom.enabled` is false.

---

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `10` | Config error — including the first run, which creates the config and stops |
| `11` | Authentication error (`auth canvas`) |
| `12` | Any other runtime failure: network, API, browser, or filesystem |

Failure handling during `sync` is uneven, and worth knowing:

- A **file** that cannot be fetched or downloaded is logged, recorded in
  `state.json` with an incremented `error_count`, and skipped. The run
  continues and still exits `0`. Use `status --verbose` to find these.
- The **announcements** and **Zoom** stages are each wrapped per course: if one
  fails, it is logged and the next course proceeds.
- A **page or assignment** that cannot be fetched aborts the entire run,
  including courses not yet reached, and exits `12`.

That last case is a rough edge rather than a deliberate design: a single
unreadable page stops everything. Re-running is safe and skips what already
succeeded, but if one course consistently fails this way, exclude it with
`canvas.ignored_courses` or sync the others with `--course-id`.

---

## Troubleshooting

**`created example config at ... Please edit it.`**
Expected on first run. Edit the file, then re-run.

**`missing or invalid fields in config: [...]`**
The listed keys failed validation. See [Validation](#validation).

**Canvas returns 401 or 403.**
Check that `base_url` has no trailing path and matches your instance exactly.
If using `token_cmd`, run it yourself — it must print the token and nothing
else. Note that tokens are per-instance.

**Rate limiting, or slow syncs.**
Lower `max_rps` to `1` and `concurrency` to `2`. The client already honours
`Retry-After` on 429 (capped at 60s) and backs off exponentially on 5xx, up to
5 attempts per request.

**The Zoom login stalls or captures nothing.**
Confirm `sso_email` and `sso_password` are correct, that `external_tool_id`
matches your Canvas, and that Chrome or Chromium is installed. Then set
`logging.level = "debug"` and read the log file to see which step stopped.

**Recordings are listed but fail to download.**
Confirm `ffmpeg -version` works. Asset URLs are short-lived; if a batch times
out, re-run — already-downloaded recordings are skipped.

**Some files fail every run.**
`u_crawler status --verbose` lists them with their errors. Re-running retries
them; partial downloads resume rather than restart.

**Nothing appears in `status`.**
It reads `state.json` under `download_root`. If you changed `download_root`
after a sync, it is looking in the new location.

---

## Development

```bash
cargo fmt --all
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test
```

CI runs all three, plus release builds for Linux and Windows. `Cargo.toml`
carries a `[lints.clippy]` table so local runs deny the same lints CI does.

`AGENTS.md` documents the internal architecture and the invariants the code
holds to. Some worth knowing before changing anything:

- All HTTP goes through `HttpCtx`. A raw `reqwest::Client::send()` outside
  `src/http.rs` bypasses rate limiting and retries.
- All Canvas file downloads go through `download::download_if_needed`, under
  one state-key namespace, so a file is never fetched twice.
- Nothing outside `main.rs` writes to stdout. Library code emits `tracing`
  events; user-facing output goes through the `status!` macro.
- Credential values are never logged — only their presence.

---

## License

No license has been declared for this project yet. Until one is added, all
rights are reserved by default and the code carries no permission to reuse it.
