//! User-facing terminal output.
//!
//! Distinct from `tracing`, which records diagnostics for the log file. This is
//! the channel the person running the command reads.
//!
//! Everything goes through the shared [`MultiProgress`] that [`crate::progress`]
//! registers its bars with. Writing to stdout directly while a progress bar is
//! drawing corrupts the display, which is why `println!` is not used here — nor
//! anywhere outside `main`.

use indicatif::MultiProgress;
use std::sync::OnceLock;

static BARS: OnceLock<MultiProgress> = OnceLock::new();

/// The process-wide progress-bar group.
pub fn bars() -> &'static MultiProgress {
    BARS.get_or_init(MultiProgress::new)
}

/// Prints a line above any active progress bars.
pub fn line(msg: impl AsRef<str>) {
    // A failure here means stdout is gone; there is nowhere to report that.
    let _ = bars().println(msg.as_ref());
}

/// Prints a status line to the user. Same formatting syntax as `println!`.
#[macro_export]
macro_rules! status {
    ($($arg:tt)*) => { $crate::ui::line(format!($($arg)*)) };
}
