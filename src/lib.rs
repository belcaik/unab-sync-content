//! Mirrors Canvas LMS courses and their Zoom recordings to a local directory.
//!
//! The crate is a library with a thin binary (`src/main.rs`) on top. The layers:
//!
//! - [`config`] loads and validates the user's `config.toml`.
//! - [`http`] owns the one HTTP client, its rate limiting and its retries; every
//!   request in the crate goes through it.
//! - [`canvas`] and [`zoom::api`] are the two REST clients.
//! - [`download`] is the only downloader: one ETag/resume/atomic-rename policy.
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
/// Filesystem helpers: name sanitization and atomic writes.
pub mod fsutil;
/// The shared HTTP client, with rate limiting and bounded retries.
pub mod http;
/// Extraction of links and media references from Canvas HTML.
pub mod links;
/// Log file setup.
pub mod logger;
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
pub mod zoom;
