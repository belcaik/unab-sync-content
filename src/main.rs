mod canvas;
mod config;
mod cookies;
mod ffmpeg;
mod fsutil;
mod http;
mod logger;
mod recordings;
mod state;
mod syncer;
mod zoom;

use clap::{ArgGroup, Parser, Subcommand};
use config::{load_config_from_path, save_config_to_path, Config, ConfigPaths};
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
    /// Only process and download Zoom recordings
    Recordings {
        /// Run only for a specific course id
        #[arg(long)]
        course_id: Option<u64>,
        /// Do not download; only list discovered links
        #[arg(long)]
        dry_run: bool,
    },
    /// Operaciones avanzadas con Zoom (CDP, listados, descargas)
    Zoom {
        #[command(subcommand)]
        command: ZoomCommands,
    },
    /// Show last run, pending items, failed jobs
    Status,
    /// Verify checksums, remove .part leftovers
    Clean,
}

#[derive(Subcommand, Debug)]
enum AuthCommands {
    /// Configure Canvas Personal Access Token
    Canvas(CanvasAuthArgs),
}

#[derive(Subcommand, Debug)]
enum ZoomCommands {
    #[command(name = "sniff-cdp")]
    SniffCdp {
        #[arg(long)]
        course_id: u64,
        #[arg(long, default_value = "9222")]
        debug_port: u16,
        #[arg(long)]
        keep_tab: bool,
    },
    List {
        #[arg(long)]
        course_id: u64,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        json: bool,
    },
    #[command(name = "fetch-urls")]
    FetchUrls {
        #[arg(long)]
        course_id: u64,
    },
    Dl {
        #[arg(long)]
        course_id: u64,
        #[arg(long, default_value = "1")]
        concurrency: usize,
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

    // Attempt to init logging from config before executing command.
    // If config missing, fall back to defaults.
    {
        if let Ok(paths) = config::ConfigPaths::default() {
            if let Ok(mut cfg) = config::load_config_sync(&paths.config_file) {
                cfg.expand_paths();
                logger::init_logging(Some(&cfg));
            } else {
                logger::init_logging(None);
            }
        } else {
            logger::init_logging(None);
        }
    }

    match cli.command {
        Commands::Init => {
            match handle_init().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "init failed");
                    eprintln!("error: {e}");
                    ExitCode::from(10) // config error
                }
            }
        }
        Commands::Auth(AuthCommands::Canvas(args)) => {
            match handle_auth_canvas(args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "auth canvas failed");
                    eprintln!("error: {e}");
                    ExitCode::from(11) // auth error
                }
            }
        }
        Commands::Scan { course_id } => {
            if let Err(e) = handle_scan(course_id).await {
                tracing::error!(error = %e, course_id = ?course_id, "scan failed");
                eprintln!("error: {e}");
                return ExitCode::from(12); // network
            }
            ExitCode::SUCCESS
        }
        Commands::Sync {
            course_id,
            dry_run,
            verbose,
        } => match syncer::run_sync(course_id, dry_run, verbose).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!(error = %e, "sync failed");
                eprintln!("error: {e}");
                ExitCode::from(12)
            }
        },
        Commands::Recordings { course_id, dry_run } => {
            match recordings::run_discovery(course_id, dry_run).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "recordings discovery failed");
                    eprintln!("error: {e}");
                    ExitCode::from(12)
                }
            }
        }
        Commands::Zoom { command } => match command {
            ZoomCommands::SniffCdp {
                course_id,
                debug_port,
                keep_tab,
            } => match zoom::zoom_sniff_cdp(course_id, debug_port, keep_tab).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "zoom sniff-cdp failed");
                    eprintln!("error: {e}");
                    ExitCode::from(12)
                }
            },
            ZoomCommands::List {
                course_id,
                since,
                json,
            } => match zoom::zoom_list(course_id, since, json).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "zoom list failed");
                    eprintln!("error: {e}");
                    ExitCode::from(12)
                }
            },
            ZoomCommands::FetchUrls { course_id } => match zoom::zoom_fetch_urls(course_id).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "zoom fetch-urls failed");
                    eprintln!("error: {e}");
                    ExitCode::from(12)
                }
            },
            ZoomCommands::Dl {
                course_id,
                concurrency,
            } => match zoom::zoom_download(course_id, concurrency).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!(error = %e, "zoom dl failed");
                    eprintln!("error: {e}");
                    ExitCode::from(12)
                }
            },
        },
        Commands::Status => {
            println!("status: stub (implement in M5)");
            ExitCode::SUCCESS
        }
        Commands::Clean => {
            println!("clean: stub (implement in M5)");
            ExitCode::SUCCESS
        }
    }
}

async fn handle_init() -> Result<(), Box<dyn std::error::Error>> {
    let paths = ConfigPaths::default()?;
    let mut cfg = Config::default();
    cfg.expand_paths();

    tokio::fs::create_dir_all(&paths.config_dir).await?;
    save_config_to_path(&cfg, &paths.config_file).await?;
    tracing::info!(path = %paths.config_file.display(), "created config");
    println!("created config at {}", paths.config_file.display());
    Ok(())
}

async fn handle_auth_canvas(args: CanvasAuthArgs) -> Result<(), Box<dyn std::error::Error>> {
    let paths = ConfigPaths::default()?;
    tokio::fs::create_dir_all(&paths.config_dir).await?;

    // Load existing, or start from default
    let mut cfg: Config = load_config_from_path(&paths.config_file)
        .await
        .unwrap_or_default();

    if let Some(base) = args.base_url {
        cfg.canvas.base_url = base;
    }
    if let Some(token) = args.token {
        cfg.canvas.token = Some(token);
        cfg.canvas.token_cmd = None;
    }
    if let Some(cmd) = args.token_cmd {
        cfg.canvas.token_cmd = Some(cmd);
        cfg.canvas.token = None;
    }

    cfg.expand_paths();
    save_config_to_path(&cfg, &paths.config_file).await?;
    tracing::info!(path = %paths.config_file.display(), "saved canvas auth");
    println!("saved canvas auth in {}", paths.config_file.display());
    Ok(())
}

async fn handle_scan(course_id: Option<u64>) -> Result<(), Box<dyn std::error::Error>> {
    use canvas::CanvasClient;
    let client = CanvasClient::from_config().await?;

    if let Some(cid) = course_id {
        let modules = client.list_modules_with_items(cid).await?;
        println!("Modules (course_id={cid}):");
        for m in &modules {
            println!("- [{}] {} (items: {})", m.id, m.name, m.items.len());
        }
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
        println!("Courses:");
        for c in courses {
            let code = c.course_code.unwrap_or_default();
            println!(
                "- [{}] {} {}",
                c.id,
                c.name,
                if code.is_empty() {
                    "".to_string()
                } else {
                    format!("- {}", code)
                }
            );
        }
    }
    Ok(())
}
