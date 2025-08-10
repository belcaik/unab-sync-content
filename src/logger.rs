use crate::config::{Config, ConfigPaths};
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use tracing_subscriber::{fmt, EnvFilter};

pub fn init_logging(cfg: Option<&Config>) {
    let (level, file_path) = if let Some(c) = cfg {
        (c.logging.level.clone(), PathBuf::from(&c.logging.file))
    } else {
        // Fallback to default path inside config dir
        let paths = ConfigPaths::default().ok();
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

    // Open file in append mode
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .unwrap_or_else(|_| File::create(&file_path).expect("create log file"));

    let (non_blocking, _guard) = tracing_appender::non_blocking(file);
    // Leak guard so it lives for process lifetime
    Box::leak(Box::new(_guard));

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_level(true)
        .init();
}
