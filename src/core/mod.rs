//! Cross-cutting plumbing shared by every flow: configuration, the one HTTP
//! client, per-course sync state, filesystem helpers, and log-file setup.

/// Configuration file loading, validation and path expansion.
pub mod config;
/// Filesystem helpers: name sanitization and atomic writes.
pub mod fsutil;
/// The shared HTTP client, with rate limiting and bounded retries.
pub mod http;
/// Log file setup.
pub mod logger;
/// Per-course sync state: what has been fetched, and what failed trying.
pub mod state;
/// The one resumable transfer: `.part` sidecar, `Range` resume, atomic rename.
pub mod transfer;
