//! Mirrors Canvas LMS courses and their Zoom recordings to a local directory.
//!
//! The crate is a library with a thin binary (`src/main.rs`) on top. The layers:
//!
//! - [`core`] is the shared plumbing: [`core::config`] loads and validates the
//!   user's `config.toml`, [`core::http`] owns the one HTTP client with its
//!   rate limiting and retries, [`core::state`] records per-course what has
//!   been fetched and what failed, [`core::fsutil`] sanitizes paths and writes
//!   atomically, and [`core::logger`] sets up the log file.
//! - [`canvas::client`] and [`zoom::api`] are the two REST clients.
//! - [`canvas::download`] is the only downloader: one ETag/resume/atomic-rename
//!   policy.
//! - [`canvas::sync`], [`canvas::announcements`], [`canvas::calendar`] and
//!   [`canvas::recordings`] are the feature flows.
//! - [`ui`] is the user-facing output channel; [`tracing`] is the diagnostic one.
//!   Nothing outside `main` writes to stdout directly.

/// Everything that speaks Canvas: client, wire types and the feature flows.
pub mod canvas;
/// Cross-cutting plumbing: config, HTTP, state, filesystem and logging.
pub mod core;
/// Invocation of `ffmpeg`, used to fetch Zoom's streamed recordings.
pub mod ffmpeg;
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
