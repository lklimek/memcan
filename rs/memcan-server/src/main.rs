//! MemCan — unified server binary with MCP transport + admin subcommands.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use memcan_core::error::MemcanError;

mod import_triaged;
mod index_code;
mod index_standards;
mod migrate;
mod serve;
mod test_classification;

#[derive(Parser)]
#[command(
    name = "memcan-server",
    about = "MemCan — persistent memory for Claude Code",
    version = env!("MEMCAN_VERSION"),
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the MCP server (default when no subcommand given).
    Serve(ServeArgs),

    /// Index source code into vector store.
    IndexCode(IndexCodeArgs),

    /// Index markdown standards documents into vector store.
    IndexStandards(IndexStandardsArgs),

    /// Import JSON export into LanceDB vector store.
    Migrate(MigrateArgs),

    /// Import triaged memory candidates from a report file.
    ImportTriaged(ImportTriagedArgs),

    /// Replay hook data log for prompt classification tuning.
    TestClassification(TestClassificationArgs),

    /// Download the configured embedding model and exit.
    DownloadModel(DownloadModelArgs),

    /// Generate shell completions.
    Completions(CompletionsArgs),
}

#[derive(Parser)]
pub struct ServeArgs {
    /// Run in stdio mode (backward compat with Claude Code subprocess).
    #[arg(long)]
    pub stdio: bool,

    /// Listen address (overrides MEMCAN_LISTEN env var).
    #[arg(long)]
    pub listen: Option<String>,
}

#[derive(Parser)]
pub struct IndexCodeArgs {
    /// Root directory of the project to index.
    pub project_dir: PathBuf,

    /// Project name for payload tagging.
    #[arg(long)]
    pub project: String,

    /// Tech stack label.
    #[arg(long)]
    pub tech_stack: Option<String>,

    /// Drop all indexed data for this project.
    #[arg(long)]
    pub drop: bool,

    /// Enable debug logging.
    #[arg(long)]
    pub verbose: bool,

    /// Skip files larger than this many bytes.
    #[arg(long, default_value = "1048576")]
    pub max_file_size: u64,
}

#[derive(Parser)]
pub struct IndexStandardsArgs {
    /// Markdown file to index (required unless --drop).
    pub file: Option<PathBuf>,

    /// Standard identifier.
    #[arg(long)]
    pub standard_id: String,

    /// Type of standard (security, coding, cve, guideline).
    #[arg(long)]
    pub standard_type: Option<String>,

    /// Standard version.
    #[arg(long)]
    pub version: Option<String>,

    /// Language code.
    #[arg(long)]
    pub lang: Option<String>,

    /// Technology stack.
    #[arg(long)]
    pub tech_stack: Option<String>,

    /// Source URL.
    #[arg(long)]
    pub url: Option<String>,

    /// LLM model for metadata extraction.
    #[arg(long)]
    pub model: Option<String>,

    /// Drop all points for --standard-id.
    #[arg(long)]
    pub drop: bool,

    /// Resume from chunk index.
    #[arg(long, default_value = "0")]
    pub retry_from: usize,

    /// Enable debug logging.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Parser)]
pub struct MigrateArgs {
    /// Path to JSON export file.
    pub export_file: PathBuf,

    /// Print plan without writing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Parser)]
pub struct ImportTriagedArgs {
    /// Path to triaged report.json.
    pub report: PathBuf,

    /// Show what would be imported without storing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Parser)]
pub struct TestClassificationArgs {
    /// Path to fact-extraction prompt .md file.
    #[arg(long)]
    pub prompt: PathBuf,

    /// LLM model name.
    #[arg(long)]
    pub model: String,

    /// Path to JSONL data file.
    #[arg(long)]
    pub data: Option<PathBuf>,
}

#[derive(Parser)]
pub struct DownloadModelArgs {
    /// Embedding model name override.
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Parser)]
pub struct CompletionsArgs {
    /// Shell to generate completions for.
    pub shell: clap_complete::Shell,
}

fn setup_tracing(verbose: bool) {
    let filter = if verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .init();
}

#[tokio::main]
async fn main() -> Result<(), MemcanError> {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Command::Serve(_)) => {
            let args = match cli.command {
                Some(Command::Serve(a)) => a,
                _ => ServeArgs {
                    stdio: false,
                    listen: None,
                },
            };
            serve::run(&args).await
        }
        Some(Command::IndexCode(args)) => {
            setup_tracing(args.verbose);
            index_code::run(&args).await
        }
        Some(Command::IndexStandards(args)) => {
            setup_tracing(args.verbose);
            index_standards::run(&args).await
        }
        Some(Command::Migrate(args)) => {
            setup_tracing(false);
            migrate::run(&args).await
        }
        Some(Command::ImportTriaged(args)) => {
            setup_tracing(false);
            import_triaged::run(&args).await
        }
        Some(Command::TestClassification(args)) => {
            setup_tracing(false);
            test_classification::run(&args).await
        }
        Some(Command::DownloadModel(_args)) => {
            let (settings, _embedder) =
                memcan_core::init::MemcanContext::init_settings_and_embedder()?;
            println!(
                "Embedding model '{}' ({}d) ready.",
                settings.embed_model, settings.embed_dims
            );
            Ok(())
        }
        Some(Command::Completions(args)) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(
                args.shell,
                &mut cmd,
                "memcan-server",
                &mut std::io::stdout(),
            );
            Ok(())
        }
    }
}
