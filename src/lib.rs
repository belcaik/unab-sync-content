//! Mirrors Canvas LMS courses and their Zoom recordings to a local directory.
//!
//! The crate is a library with a thin binary (`src/main.rs`) on top. The layers:
//!
//! - [`core`] is the shared plumbing: [`core::config`] loads and validates the
//!   user's `config.toml`, [`core::http`] owns the one HTTP client with its
//!   rate limiting and retries, [`core::transfer`] owns the one resumable
//!   download, [`core::state`] records per-course what has been fetched and
//!   what failed, [`core::fsutil`] sanitizes paths and writes atomically, and
//!   [`core::logger`] sets up the log file.
//! - [`canvas::client`] and [`zoom::api`] are the two REST clients.
//! - [`canvas::course`] names the per-course on-disk directory and selects the
//!   courses every command works on.
//! - [`canvas::download`] adds the ETag/state policy for Canvas files on top
//!   of [`core::transfer`].
//! - [`canvas::sync`], [`canvas::announcements`], [`canvas::calendar`],
//!   [`canvas::recordings`] and [`canvas::status`] are the feature flows;
//!   [`app`] orchestrates them per course, so Canvas code does not depend on
//!   Zoom.
//! - [`ui`] is the user-facing output channel; [`tracing`] is the diagnostic one.
//!   Nothing outside `main` writes to stdout directly.

/// The top-level orchestrator: one config load, one course loop, each flow in turn.
pub mod app;
/// Everything that speaks Canvas: client, wire types and the feature flows.
pub mod canvas;
/// Cross-cutting plumbing: config, HTTP, state, filesystem and logging.
pub mod core;
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
