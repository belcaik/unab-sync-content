use u_crawler::app;
use u_crawler::canvas;
use u_crawler::canvas::announcements;
use u_crawler::canvas::calendar;
use u_crawler::canvas::recordings;
use u_crawler::canvas::status::{self, StatusReport};
use u_crawler::core::config;
use u_crawler::core::logger;
use u_crawler::ui::progress;
#[cfg(feature = "zoom")]
use u_crawler::zoom;

use clap::{ArgGroup, Parser, Subcommand};
use config::{load_config_from_path, save_config_to_path, Config, ConfigError, ConfigPaths};
use progress::progress_bar;
use std::process::ExitCode;

/// u_crawler — Canvas/Zoom course backup CLI
#[derive(Parser, Debug)]
#[command(
    name = "u_crawler",
    version,
    about = "Canvas/Zoom course backup CLI",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create default config and paths
    Init,
    /// Authenticate providers (Canvas PAT, etc.)
    #[command(subcommand)]
    Auth(AuthCommands),
    /// Enumerate courses/modules/files; dry-run
    Scan {
        /// Optional course id to filter
        #[arg(long)]
        course_id: Option<u64>,
    },
    /// Incremental download of Canvas files and Zoom recordings
    Sync {
        /// Sync a specific course by id
        #[arg(long)]
        course_id: Option<u64>,
        /// Do not write files or state; show planned actions
        #[arg(long)]
        dry_run: bool,
        /// Print extra info (e.g., skipped items)
        #[arg(long)]
        verbose: bool,
    },
    /// Download and index course announcements (with link/media extraction)
    Announcements {
        /// Run only for a specific course id
        #[arg(long)]
        course_id: Option<u64>,
        /// Do not write files or state; show planned actions
        #[arg(long)]
        dry_run: bool,
    },
    /// Project assignment deadlines to the caldir tree as .ics files
    Calendar {
        /// Run only for a specific course id
        #[arg(long)]
        course_id: Option<u64>,
        /// Do not write files; show planned actions
        #[arg(long)]
        dry_run: bool,
    },
    /// Only process and download Zoom recordings
    Recordings {
        /// Run only for a specific course id
        #[arg(long)]
        course_id: Option<u64>,
        /// Do not download; only list discovered links
        #[arg(long)]
        dry_run: bool,
    },
    /// Advanced Zoom operations (CDP, listing, downloads)
    Zoom {
        #[command(subcommand)]
        command: ZoomCommands,
    },
    /// Show last run, pending items, failed jobs
    Status {
        /// Show detailed information including failed items
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCommands {
    /// Configure Canvas Personal Access Token
    Canvas(CanvasAuthArgs),
}

#[derive(Subcommand, Debug)]
enum ZoomCommands {
    #[command(name = "flow")]
    Flow {
        #[arg(long)]
        course_id: u64,
        #[arg(long)]
        since: Option<String>,
    },
}

#[derive(Parser, Debug)]
#[command(group(
    ArgGroup::new("token-src")
        .required(true)
        .args(["token", "token_cmd"]) 
))]
struct CanvasAuthArgs {
    /// Canvas base URL, e.g. https://<tenant>.instructure.com
    #[arg(long)]
    base_url: Option<String>,
    /// Personal Access Token value
    #[arg(long)]
    token: Option<String>,
    /// Command to retrieve token (e.g., `pass show canvas/pat`)
    #[arg(long)]
    token_cmd: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // Initialise logging from config before executing the command; fall back to
    // defaults when the config is missing or invalid.
    {
        match config::Config::load_or_init() {
            Ok(cfg) => logger::init_logging(Some(&cfg)),
            Err(config::ConfigError::MissingConfigFile(path)) => {
                eprintln!(
                    "u_crawler: created example config at {}. Please edit it.",
                    path
                );
                // We exit immediately so the user notices they need to edit the config.
                // This applies to all commands (since clap parsed args already).
                std::process::exit(10);
            }
            Err(_) => {
                // Other errors (e.g. validation) will be caught later by specific commands
                // or we just fall back to default logging.
                logger::init_logging(None);
            }
        }
    }

    match cli.command {
        Commands::Init => exit_on(handle_init().await, "init failed", 10),
        Commands::Auth(AuthCommands::Canvas(args)) => {
            exit_on(handle_auth_canvas(args).await, "auth canvas failed", 11)
        }
        Commands::Scan { course_id } => {
            let result = handle_scan(course_id).await;
            if result.is_err() {
                tracing::error!(course_id = ?course_id, "scan failed for course filter");
            }
            exit_on(result, "scan failed", 12)
        }
        Commands::Sync {
            course_id,
            dry_run,
            verbose,
        } => {
            let result: anyhow::Result<()> = async {
                let cfg = Config::load_or_init()?;
                app::run_sync(&cfg, course_id, dry_run, verbose).await
            }
            .await;
            exit_on(result, "sync failed", 12)
        }
        Commands::Announcements { course_id, dry_run } => {
            let result: anyhow::Result<()> = async {
                let cfg = Config::load_or_init()?;
                announcements::run_discovery(&cfg, course_id, dry_run).await
            }
            .await;
            exit_on(result, "announcements failed", 12)
        }
        Commands::Calendar { course_id, dry_run } => {
            let result: anyhow::Result<calendar::RunOutcome> = async {
                let cfg = Config::load_or_init()?;
                calendar::run_calendar(&cfg, course_id, dry_run).await
            }
            .await;
            match result {
                Ok(calendar::RunOutcome::Success) => ExitCode::SUCCESS,
                Ok(calendar::RunOutcome::PartialFailure) => {
                    eprintln!("warning: some courses failed to sync; see the log for details");
                    ExitCode::from(13) // partial failure: some courses failed
                }
                Err(e) => exit_on(Err(e), "calendar sync failed", 12),
            }
        }
        Commands::Recordings { course_id, dry_run } => {
            let result: anyhow::Result<()> = async {
                let cfg = Config::load_or_init()?;
                recordings::run_discovery(&cfg, course_id, dry_run).await
            }
            .await;
            exit_on(result, "recordings discovery failed", 12)
        }
        #[cfg(feature = "zoom")]
        Commands::Zoom { command } => match command {
            ZoomCommands::Flow { course_id, since } => {
                let result: anyhow::Result<()> = async {
                    let cfg = Config::load_or_init()?;
                    zoom::zoom_flow(&cfg, course_id, since).await
                }
                .await;
                exit_on(result, "zoom flow failed", 12)
            }
        },
        #[cfg(not(feature = "zoom"))]
        Commands::Zoom { .. } => {
            eprintln!(
                "error: this build of u_crawler was compiled without the `zoom` feature; \
                 the Zoom flow (headless browser, SSO capture, recording download) is not \
                 available. Rebuild with the default features enabled to use it."
            );
            ExitCode::from(12)
        }
        Commands::Status { verbose } => exit_on(handle_status(verbose).await, "status failed", 12),
    }
}

fn exit_on(result: anyhow::Result<()>, what: &str, code: u8) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "{what}");
            eprintln!("error: {e}");
            ExitCode::from(code)
        }
    }
}

async fn handle_init() -> anyhow::Result<()> {
    match Config::load_or_init() {
        Ok(_paths) => {
            println!("Config file already exists and is valid.");
        }
        Err(ConfigError::MissingConfigFile(path)) => {
            println!("Created default config at {}", path);
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

async fn handle_auth_canvas(args: CanvasAuthArgs) -> anyhow::Result<()> {
    let paths = ConfigPaths::new()?;

    // Load or init, but if it was just created (MissingConfigFile), we proceed with default config
    let mut cfg = match Config::load_or_init() {
        Ok(c) => c,
        Err(ConfigError::MissingConfigFile(_)) => {
            // It was just created, load it again (it's default)
            load_config_from_path(&paths.config_file).await?
        }
        Err(e) => return Err(e.into()),
    };

    if let Some(base) = args.base_url {
        cfg.canvas.base_url = base;
    }
    if let Some(token) = args.token {
        cfg.canvas.set_token(token);
    }
    if let Some(cmd) = args.token_cmd {
        cfg.canvas.set_token_cmd(cmd);
    }

    cfg.expand_paths();
    save_config_to_path(&cfg, &paths.config_file).await?;
    tracing::info!(path = %paths.config_file.display(), "saved canvas auth");
    println!("saved canvas auth in {}", paths.config_file.display());
    Ok(())
}

async fn handle_scan(course_id: Option<u64>) -> anyhow::Result<()> {
    use canvas::CanvasClient;
    let cfg = Config::load_or_init()?;
    let client = CanvasClient::new(&cfg).await?;

    if let Some(cid) = course_id {
        let modules = client.list_modules_with_items(cid).await?;
        let pb = progress_bar(modules.len() as u64, &format!("Modules for course {cid}"));
        pb.println(format!("Modules (course_id={cid}):"));
        for m in &modules {
            pb.inc(1);
            pb.println(format!(
                "- [{}] {} (items: {})",
                m.id,
                m.name,
                m.items.len()
            ));
        }
        pb.finish_and_clear();
        // Derive files via module items to avoid list_files 403
        let mut file_count = 0usize;
        for m in &modules {
            for it in &m.items {
                if matches!(it.kind.as_deref(), Some("File")) {
                    file_count += 1;
                }
            }
        }
        println!("Files (discovered via modules) count: {}", file_count);
    } else {
        let courses = client.list_courses().await?;
        let pb = progress_bar(courses.len() as u64, "Courses");
        pb.println("Courses:");
        for c in courses {
            let code = c.course_code.unwrap_or_default();
            pb.inc(1);
            pb.println(format!(
                "- [{}] {} {}",
                c.id,
                c.name,
                if code.is_empty() {
                    "".to_string()
                } else {
                    format!("- {}", code)
                }
            ));
        }
        pb.finish_and_clear();
    }
    Ok(())
}

async fn handle_status(verbose: bool) -> anyhow::Result<()> {
    let cfg = Config::load_or_init()?;
    let download_root = std::path::PathBuf::from(&cfg.download_root);

    match status::summarize(&download_root).await? {
        StatusReport::NoBackupRoot => {
            println!("No backup directory found at {}", download_root.display());
            println!("Run 'u_crawler sync' to create your first backup.");
        }
        StatusReport::NoCourses => {
            println!("No courses found in {}", download_root.display());
            println!("Run 'u_crawler sync' to create your first backup.");
        }
        StatusReport::Courses(summary) => {
            println!("Backup Status:\n");

            for course in &summary.courses {
                println!("Course: {}", course.name);
                println!("  Files: {}", course.file_count);
                println!("  Storage: {}", format_bytes(course.storage_bytes));
                match &course.last_updated {
                    Some(timestamp) => println!("  Last sync: {}", timestamp),
                    None => println!("  Last sync: Never"),
                }

                if !course.failed_items.is_empty() {
                    println!(
                        "  Failed downloads: {} items need retry",
                        course.failed_items.len()
                    );
                    if verbose {
                        for item in &course.failed_items {
                            println!("    - {} (failed {} time(s))", item.key, item.error_count);
                            if let Some(err) = &item.last_error {
                                println!("      Error: {}", truncate_chars(err, 60));
                            }
                        }
                    } else {
                        println!("      Run with --verbose to see details");
                    }
                }

                println!();
            }

            println!("─────────────────────────────");
            println!(
                "Total: {} courses, {} files, {}",
                summary.courses.len(),
                summary.total_files,
                format_bytes(summary.total_storage)
            );
            println!();
            println!("Tip: Run 'u_crawler sync --dry-run' to check for remote changes");
        }
    }

    Ok(())
}

/// Truncates `s` to at most `max` characters, appending an ellipsis when shortened.
///
/// Operates on `char` boundaries: slicing by byte index would panic on the
/// multi-byte text that Canvas returns for non-English courses.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}...")
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_leaves_short_input_untouched() {
        assert_eq!(truncate_chars("short", 60), "short");
    }

    #[test]
    fn truncate_chars_appends_ellipsis_when_shortened() {
        assert_eq!(truncate_chars("abcdef", 3), "abc...");
    }

    #[test]
    fn truncate_chars_does_not_split_multibyte_characters() {
        // Byte-slicing this at 3 would panic mid-codepoint.
        let input = "áéíóú";
        assert_eq!(truncate_chars(input, 3), "áéí...");
    }

    #[test]
    fn format_bytes_scales_to_the_largest_fitting_unit() {
        assert_eq!(format_bytes(512), "512 bytes");
        assert_eq!(format_bytes(2048), "2.00 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.00 MB");
    }
}
