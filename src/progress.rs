use indicatif::{ProgressBar, ProgressStyle};

fn default_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.blue} {msg} [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
        .unwrap()
        .progress_chars("##-")
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.blue} {msg}").unwrap()
}

pub fn progress_bar(len: u64, message: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(default_style());
    pb.set_message(message.to_string());
    pb
}

pub fn spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(spinner_style());
    pb.set_message(message.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(100));
    pb
}
