use crate::config::{Config, ConfigPaths};
use std::fs::OpenOptions;
use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

pub fn init_logging(cfg: Option<&Config>) {
    let (level, file_path) = if let Some(c) = cfg {
        (c.logging.level.clone(), PathBuf::from(&c.logging.file))
    } else {
        // Fallback to default path inside config dir
        let paths = ConfigPaths::new().ok();
        let p = paths
            .as_ref()
            .map(|p| p.config_dir.join("u_crawler.log"))
            .unwrap_or_else(|| PathBuf::from("u_crawler.log"));
        ("info".to_string(), p)
    };

    // Ensure parent dir exists
    if let Some(parent) = file_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));

    // An unwritable log path must not stop the program: fall back to stderr.
    let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
    else {
        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .init();
        tracing::warn!(path = %file_path.display(), "could not open log file; logging to stderr");
        return;
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(file);
    // The guard flushes the appender on drop, so it must outlive every log call.
    // Leaking it ties that to the process lifetime.
    Box::leak(Box::new(guard));

    fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .init();
}
