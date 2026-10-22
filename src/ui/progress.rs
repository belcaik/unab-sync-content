//! Progress bars, registered with the shared group in [`crate::ui`] so that
//! status lines can be printed without corrupting them.

use crate::ui;
use indicatif::{ProgressBar, ProgressStyle};

fn default_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.blue} {msg} [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("##-")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.blue} {msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

pub fn progress_bar(len: u64, message: &str) -> ProgressBar {
    let pb = ui::bars().add(ProgressBar::new(len));
    pb.set_style(default_style());
    pb.set_message(message.to_string());
    pb
}

pub fn spinner(message: &str) -> ProgressBar {
    let pb = ui::bars().add(ProgressBar::new_spinner());
    pb.set_style(spinner_style());
    pb.set_message(message.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb
}
