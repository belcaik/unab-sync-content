//! Mirrors Canvas LMS courses and their Zoom recordings to a local directory.
//!
//! The crate is a library with a thin binary (`src/main.rs`) on top. The layers:
//!
//! - [`config`] loads and validates the user's `config.toml`.
//! - [`http`] owns the one HTTP client, its rate limiting and its retries; every
//!   request in the crate goes through it.
//! - [`canvas`] and [`zoom::api`] are the two REST clients.
//! - [`download`] is the only downloader: one ETag/resume/atomic-rename policy.
//! - [`layout`] turns course/module/item metadata into on-disk names; it is
//!   pure, so the writers and the offline migrator agree on every path.
//! - [`frontmatter`] renders the YAML block on a generated note and updates it
//!   in place.
//! - [`links`] extracts URLs from Canvas HTML and rewrites Canvas `/files/<id>`
//!   links in markdown to local paths.
//! - [`index`] renders the layout v2 index files (per-course `README.md` and
//!   `curso.json`, per-module `README.md`, root `INDEX.md` and `AGENTS.md`)
//!   from an in-memory model both `sync` and `migrate` can build.
//! - [`migrate`] scans a layout v1 library and plans its move to v2 without
//!   deleting or overwriting a file. [`migrate::rewrite`] is the writer: it
//!   copies a generated note under `.u_crawler/backup/` and then replaces it.
//! - [`library`] reads the library root's `curso.json` files and tells every
//!   writer which directory a course owns, so they all agree.
//! - [`state`] records per-course what has been fetched and what failed.
//! - [`syncer`], [`announcements`] and [`recordings`] are the feature flows.
//! - [`ui`] is the user-facing output channel; [`tracing`] is the diagnostic one.
//!   Nothing outside `main` writes to stdout directly.

/// Course announcements: fetched, rendered to markdown, and indexed.
pub mod announcements;
/// The calendar-sync flow: a pure deadline planner plus its I/O executor.
pub mod calendar;
/// The Canvas LMS REST client.
pub mod canvas;
/// Configuration file loading, validation and path expansion.
pub mod config;
/// The single downloader for Canvas-hosted files.
pub mod download;
/// Invocation of `ffmpeg`, used to fetch Zoom's streamed recordings.
pub mod ffmpeg;
/// YAML frontmatter for pages, assignments, announcements and recordings.
pub mod frontmatter;
/// Filesystem helpers: name sanitization and atomic writes.
pub mod fsutil;
/// The shared HTTP client, with rate limiting and bounded retries.
pub mod http;
/// Layout v2 index files: pure rendering of the per-course and library-level
/// `README.md`, `curso.json`, `INDEX.md` and `AGENTS.md`.
pub mod index;
/// Library layout v2: pure metadata-to-path naming, the one source of truth for paths.
pub mod layout;
/// Which directory each course owns in the library (`curso.json`), shared by the writers.
pub mod library;
/// Extraction of links from Canvas HTML, and rewriting of Canvas file links in markdown.
pub mod links;
/// Log file setup.
pub mod logger;
/// Retroactive layout v1 → v2 migration: read-only scan, pure detection and
/// planning, plus [`migrate::rewrite`] for generated markdown.
pub mod migrate;
/// Progress bars.
pub mod progress;
/// Discovery of Zoom links referenced across a course.
pub mod recordings;
/// Per-course sync state: what has been fetched, and what failed trying.
pub mod state;
/// The main sync: courses, modules, pages, assignments and their files.
pub mod syncer;
/// User-facing terminal output.
pub mod ui;
/// Zoom recordings: session capture, listing and download.
///
/// Gated behind the `zoom` cargo feature (default-on). It is the only part of
/// the crate that pulls in `chromiumoxide`, a git-pinned headless browser
/// dependency — building with `--no-default-features` drops it, and the
/// calendar-sync flow works completely without it.
#[cfg(feature = "zoom")]
pub mod zoom;
