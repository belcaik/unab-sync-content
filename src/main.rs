mod config;

use clap::{ArgGroup, Parser, Subcommand};
use config::{load_config_from_path, save_config_to_path, Config, ConfigPaths};
use std::process::ExitCode;

/// u_crawler — Canvas/Zoom course backup CLI
#[derive(Parser, Debug)]
#[command(name = "u_crawler", version, about = "Canvas/Zoom course backup CLI", propagate_version = true)]
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
    Sync,
    /// Only process and download Zoom recordings
    Recordings,
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

    match cli.command {
        Commands::Init => {
            match handle_init().await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(10) // config error
                }
            }
        }
        Commands::Auth(AuthCommands::Canvas(args)) => {
            match handle_auth_canvas(args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(11) // auth error
                }
            }
        }
        Commands::Scan { course_id } => {
            println!(
                "scan: stub — course_id={:?} (implement in M1)",
                course_id
            );
            ExitCode::SUCCESS
        }
        Commands::Sync => {
            println!("sync: stub (implement in M2)");
            ExitCode::SUCCESS
        }
        Commands::Recordings => {
            println!("recordings: stub (implement in M3/M4)");
            ExitCode::SUCCESS
        }
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
    println!("created config at {}", paths.config_file.display());
    Ok(())
}

async fn handle_auth_canvas(args: CanvasAuthArgs) -> Result<(), Box<dyn std::error::Error>> {
    let paths = ConfigPaths::default()?;
    tokio::fs::create_dir_all(&paths.config_dir).await?;

    // Load existing, or start from default
    let mut cfg = match load_config_from_path(&paths.config_file).await {
        Ok(c) => c,
        Err(_) => Config::default(),
    };

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
    println!("saved canvas auth in {}", paths.config_file.display());
    Ok(())
}
