# agents.md

## Project: `u_crawler` — Canvas/Zoom course backup CLI

### Mission

Build a fast, robust CLI that authenticates to Canvas with a Personal Access Token (PAT), enumerates courses/modules/files, and downloads content to a structured folder tree. Detect Zoom class recording links in Canvas content and download media via `ffmpeg` when the user has permission (authenticated via browser-exported cookies). Respect rate limits and terms of service.

---

## Non-negotiables
* **Idempotent & resumable:** Safe retries, `Range` requests, `.part` files, checksum/ETag validation.
* **Deterministic structure:** Stable, sanitized paths; week folding rules.
* **Observability:** Structured logs, progress bars, clear exit codes.
* **Security:** Never log secrets. Support keychain or external secret command.

---

## Tech Stack

* **Language:** Rust (Edition 2021)
* **Async runtime:** `tokio`
* **HTTP:** `reqwest` (+ gzip/brotli/deflate, streaming)
* **CLI:** `clap` (derive)
* **Config:** TOML (`directories`, `toml`)
* **Parsing:** `serde`, `serde_json`, `regex`, `url`
* **Filesystem:** `tokio::fs`, `sanitize-filename`
* **UX:** `indicatif` progress
* **Process:** `which`, `tokio::process` (for `ffmpeg`)
* **Optional:** `keyring` for secrets

---

## Repository Conventions

* **Branching:** `main` (default). Feature branches: `feat/<scope>-<short-desc>`.
* **Conventional Commits:**  
  Use prefixes: `feat:`, `fix:`, `docs:`, `refactor:`, `perf:`, `test:`, `build:`, `ci:`, `chore:`, `revert:`  
  Optionally, prepend [Gitmojis](https://gitmoji.dev/) for clarity and fun (e.g., `✨ feat(canvas): list courses and paginate via Link rel=next`).
* **Formatting & Lint:** `rustfmt`, `clippy` (CI must pass).
* **Tests:** `cargo test` (unit + integration); network calls behind thin interfaces for mocking.
* **Releases:** Git tags `vMAJOR.MINOR.PATCH`; changelog via conventional commits.
* **PR Rules:** Small, reviewed, CI green, include tests/docs if applicable.


---

## CLI Spec

### Binary

`u_crawler`

### Commands

* `init`

  * Create default config and paths.
* `auth canvas --token <PAT> | --token-cmd <cmd>`

  * Store Canvas PAT or command to retrieve it.
* `scan`

  * Enumerate courses/modules/files; dry-run, no writes.
* `sync`

  * Incremental download of Canvas files and Zoom recordings.
* `recordings`

  * Only process and download Zoom recordings.
* `status`

  * Show last run, pending items, failed jobs.
* `clean`

  * Verify checksums, remove `.part` leftovers.

### Exit Codes

* `0` success
* `10` config error
* `11` auth error
* `12` network/rate-limit error (exhausted)
* `13` ffmpeg missing/failure
* `14` permissions (no download right)
* `15` partial (some items failed)

---

## Config

**Path:** `~/.config/u_crawler/config.toml`

```toml
download_root = "~/Documents/UNAB/data/Canvas"
concurrency = 4
max_rps = 2
user_agent = ""
course_include = ["*"]
course_exclude = []
week_pattern = ""
naming.safe_fs = true

[canvas]
base_url = "https://<tenant>.instructure.com"
token = ""        # optional if token_cmd used
token_cmd = ""    # e.g. "pass show canvas/pat"

[zoom]
enabled = true
ffmpeg_path = "ffmpeg"
cookie_file = "~/.config/u_crawler/zoom_cookies.txt"  # Netscape format
user_agent = "Mozilla/5.0"
```

---

## Directory Layout (Downloads)

```
<download_root>/
  <Course Name - Code>/
    <Semana XX>/
      Archivos/
        <original-filename>
      Clases/
        <YYYY-MM-DD - Title>.mp4
```

* **Week inference:** Prefer module `unlock_at`/`published_at`; fallback item `created_at`; else ISO week (`%V`) of best-known date.

---

## Canvas API Contract (v1)

* Courses: `GET /api/v1/courses?enrollment_state=active&per_page=100`
* Modules (+items): `GET /api/v1/courses/{course_id}/modules?include=items&per_page=100`
* Files index: `GET /api/v1/courses/{course_id}/files?sort=updated_at&per_page=100`
* File download: use `url` or `download_url` (HEAD for size/ETag)
* Paginación: parse `Link` header (`rel="next"`)
* Backoff:

  * Honor `Retry-After`
  * Exponential backoff for 5xx
  * Cap RPS to `max_rps`
* HTTP caching:

  * Prefer `ETag`/`If-None-Match`, `If-Modified-Since`
* Downloads:

  * `.part` + `Range` resume
  * `fsync` on finalize
  * Atomic `rename` to final

---

## Zoom Recording Flow

**Scope:** Only if user has valid access and download is permitted.

1. Discover Zoom URLs inside Canvas content (pages, announcements, module external URLs):
   Match `https://*.zoom.us/rec/(share|play)/...`
2. User provides **browser-exported cookies** (Netscape format) at `zoom.cookie_file`.
3. Resolve recording page, extract HLS/MP4 URL available to the session.
4. Download via `ffmpeg` (stream copy):

```bash
ffmpeg \
  -headers "User-Agent: <UA>\r\nCookie: <COOKIE_LINE>" \
  -i "<HLS_or_MP4_URL>" \
  -c copy -map 0 -movflags +faststart "<dest>.mp4"
```

**FFmpeg presence is mandatory** (detect with `which`).

---

## High-Level Architecture

```
src/
  main.rs           # CLI entry
  config.rs         # load/save config
  http.rs           # client factory, backoff, throttle
  canvas.rs         # API calls, pagination, mapping to tasks
  zoom.rs           # URL discovery, resolve, handoff to ffmpeg
  ffmpeg.rs         # spawning, args, error mapping
  fsutil.rs         # path building, sanitization, atomic moves, status/clean
```

* All network IO async.
* Streaming downloads; bounded concurrency (`concurrency`).
* Structured log fields: `course_id`, `module_id`, `file_id`, `url`, `dest`, `attempt`.

---

## Acceptance Criteria

### `init`

* Creates config if missing; preserves if exists.
* Populates sensible defaults; expands `~`.

### `auth`

* Saves `token` or `token_cmd`. Masks secrets in logs.

### `scan`

* Lists visible courses (name + id).
* For one course (flag `--course-id`), lists modules and files with pagination.

### `sync`

* Downloads new/changed files (ETag-aware).
* Creates stable folder structure.
* Resumes partials.
* Emits summary: totals, new, updated, skipped, failed.

### `recordings`

* Scans Canvas HTML for Zoom links.
* For accessible recordings, produces playable `.mp4`.
* Skips gracefully when download is disabled; report.

### Fault tolerance

* Network 5xx → retries with backoff.
* 429 → wait `Retry-After`, continue.
* Disk full → fail item with clear error, continue others.
* On crash → rerun is safe (idempotent).

---

## Testing Plan

* **Unit:**

  * Link header parser.
  * Week inference.
  * Path sanitization.
  * Retry/backoff decision.
* **Integration (mock server):**

  * Pagination over 3+ pages.
  * ETag/304 handling.
  * Range resume.
* **E2E (manual harness):**

  * Real Canvas sandbox course with dummy files.
  * `ffmpeg` download against a public HLS test (non-Zoom) to validate pipeline.

---

## Logging & Telemetry

* Default human logs with progress bars.
* `--json` flag for machine-readable logs:

  * `level`, `ts`, `event`, `course_id`, `item_id`, `bytes`, `duration_ms`, `result`.

---

## Security

* Never print tokens/cookies.
* Support retrieving PAT via `token_cmd` (e.g., `pass`, `gopass`, `keyring`).
* Cookies file read with `0600` permissions; warn if broader.
* Do not embed cookies in config.

---

## Tasks & Milestones (for the Agent)

### M0 — Skeleton

* Scaffold crate, `clap` CLI, `init`, config load/save, `--version`, `--help`.
* CI: build + fmt + clippy + tests.

### M1 — Canvas Listing

* HTTP client, pagination, `scan` courses/modules/files.
* Unit tests: Link parsing, pagination loop.

### M2 — File Sync

* ETag/If-None-Match, Range resume (`.part`), atomic `rename`.
* Concurrency + `max_rps` throttle.
* Progress bars per file; summary.

### M3 — Zoom Discovery

* Parse Canvas HTML content items; extract Zoom URLs (regex).
* Pluggable resolver trait (future providers).

### M4 — FFmpeg Download

* Detect `ffmpeg`; spawn with headers/cookies.
* Stream copy to MP4, `+faststart`.
* Error mapping, retries on transient 4xx/5xx.

### M5 — Status & Clean

* `status` summary JSON/human.
* `clean` removes stale `.part`, verifies sizes.

### M6 — Hardening

* More tests, docs, examples, error messages, man page.

---

## Agent Operating Instructions (Codex)

* Follow this spec exactly; ask for missing constants only if required.
* Generate small, reviewable PRs per milestone.
* Write tests alongside code.
* Keep public APIs documented (`///` rustdoc).
* Use feature flags sparingly; default build must remain minimal.
* No unsafe code without justification.

---

## Example Conventional Commits

* `feat(cli): add init command to create default config`
* `feat(canvas): paginate courses and modules via Link headers`
* `perf(download): enable ranged resume with .part files`
* `fix(zoom): handle cookie file path expansion (~)`
* `docs: add usage examples for sync and recordings`
* `test(canvas): link header parser and pagination loop`
* `refactor(fs): extract path sanitization helper`

---

## Make Targets (optional)

* `make build` → `cargo build --release`
* `make test` → `cargo test`
* `make lint` → `cargo fmt -- --check && cargo clippy -- -D warnings`
* `make run` → `cargo run -- sync`

---

## Definition of Done

* Commands `init|auth|scan|sync|recordings|status|clean` implemented.
* CI green: fmt, clippy, tests.
* README with install + quickstart.
* Works on Linux/macOS; Windows not required.
* No plaintext secrets in repo or logs.
