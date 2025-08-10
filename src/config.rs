use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to determine config directory")] 
    NoConfigDir,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSer(#[from] toml::ser::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub download_root: String,
    pub concurrency: u32,
    pub max_rps: u32,
    pub user_agent: String,
    pub course_include: Vec<String>,
    pub course_exclude: Vec<String>,
    pub week_pattern: String,
    #[serde(default)]
    pub naming: Naming,
    #[serde(default)]
    pub logging: Logging,
    pub canvas: Canvas,
    pub zoom: Zoom,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Naming {
    #[serde(default = "default_true")]
    pub safe_fs: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Canvas {
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_cmd: Option<String>,
    #[serde(default)]
    pub ignored_courses: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Zoom {
    pub enabled: bool,
    pub ffmpeg_path: String,
    pub cookie_file: String,
    pub user_agent: String,
}

fn default_true() -> bool { true }

impl Default for Config {
    fn default() -> Self {
        Self {
            download_root: "~/Documents/UNAB/data/Canvas".to_string(),
            concurrency: 4,
            max_rps: 2,
            user_agent: String::new(),
            course_include: vec!["*".to_string()],
            course_exclude: vec![],
            week_pattern: String::new(),
            naming: Naming { safe_fs: true },
            logging: Logging::default(),
            canvas: Canvas {
                base_url: "https://<tenant>.instructure.com".to_string(),
                token: None,
                token_cmd: None,
                ignored_courses: vec![],
            },
            zoom: Zoom {
                enabled: true,
                ffmpeg_path: "ffmpeg".to_string(),
                cookie_file: "~/.config/u_crawler/zoom_cookies.txt".to_string(),
                user_agent: "Mozilla/5.0".to_string(),
            },
        }
    }
}

impl Config {
    /// Expand tildes in path-like fields. No-op if expansion fails.
    pub fn expand_paths(&mut self) {
        if let Some(home) = dirs_next::home_dir() {
            self.download_root = expand_tilde(&self.download_root, &home);
            self.zoom.cookie_file = expand_tilde(&self.zoom.cookie_file, &home);
            self.logging.file = expand_tilde(&self.logging.file, &home);
        }
    }
}

fn expand_tilde(input: &str, home: &Path) -> String {
    if let Some(stripped) = input.strip_prefix("~/") {
        let mut p = PathBuf::from(home);
        p.push(stripped);
        return p.to_string_lossy().to_string();
    }
    input.to_string()
}

#[derive(Clone, Debug)]
pub struct ConfigPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
}

impl ConfigPaths {
    pub fn default() -> Result<Self, ConfigError> {
        let proj = ProjectDirs::from("", "", "u_crawler").ok_or(ConfigError::NoConfigDir)?;
        let dir = proj.config_dir().to_path_buf();
        let file = dir.join("config.toml");
        Ok(ConfigPaths { config_dir: dir, config_file: file })
    }
}

pub async fn load_config_from_path(path: &Path) -> Result<Config, ConfigError> {
    let bytes = tokio::fs::read(path).await?;
    let text = String::from_utf8_lossy(&bytes);
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}

pub async fn save_config_to_path(cfg: &Config, path: &Path) -> Result<(), ConfigError> {
    let toml_text = toml::to_string_pretty(cfg)?;

    // Ensure parent exists
    if let Some(parent) = path.parent() { tokio::fs::create_dir_all(parent).await?; }

    // Write atomically-ish: write temp, then rename
    let tmp = path.with_extension("toml.part");
    tokio::fs::write(&tmp, toml_text.as_bytes()).await?;
    // Set 0600 permissions on tmp before rename when possible
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp, perm)?;
    }
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Logging {
    #[serde(default = "default_level")] 
    pub level: String,
    #[serde(default = "default_log_file")] 
    pub file: String,
}

fn default_level() -> String { "info".into() }
fn default_log_file() -> String { "~/.config/u_crawler/u_crawler.log".into() }

impl Default for Logging {
    fn default() -> Self {
        Self { level: default_level(), file: default_log_file() }
    }
}

/// Synchronous config loader (used before async runtime for logging init).
pub fn load_config_sync(path: &Path) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)?;
    let cfg: Config = toml::from_str(&text)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("u_crawler_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        let mut cfg = Config::default();
        cfg.expand_paths();
        save_config_to_path(&cfg, &path).await.unwrap();

        let loaded = load_config_from_path(&path).await.unwrap();
        assert_eq!(loaded.canvas.base_url, cfg.canvas.base_url);
        assert_eq!(loaded.zoom.enabled, cfg.zoom.enabled);
    }
}
