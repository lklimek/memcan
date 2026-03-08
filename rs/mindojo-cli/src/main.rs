//! mindojo-cli — thin MCP client for MindOJO server operations.
//!
//! No dependency on mindojo-core (no fastembed, LanceDB, or genai).
//! Communicates with the MindOJO server over HTTP via MCP protocol.

use clap::{Parser, Subcommand};

mod client;
mod extract;

#[derive(Parser)]
#[command(
    name = "mindojo-cli",
    about = "MindOJO thin CLI client",
    version = env!("CARGO_PKG_VERSION"),
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Store a memory on the server.
    Add(AddArgs),

    /// Search memories on the server.
    Search(SearchArgs),

    /// Hook handler: reads stdin (Claude Code hook payload), sends to server.
    Extract,

    /// Check queue status on the server.
    Status(StatusArgs),

    /// Count memories on the server.
    Count(CountArgs),
}

#[derive(Parser)]
struct AddArgs {
    /// Memory content to store.
    memory: String,

    /// Project scope.
    #[arg(long)]
    project: Option<String>,
}

#[derive(Parser)]
struct SearchArgs {
    /// Search query.
    query: String,

    /// Project scope.
    #[arg(long)]
    project: Option<String>,

    /// Max results.
    #[arg(long, default_value = "10")]
    limit: u32,
}

#[derive(Parser)]
struct StatusArgs {
    /// Specific operation ID.
    operation_id: Option<String>,
}

#[derive(Parser)]
struct CountArgs {
    /// Project scope.
    #[arg(long)]
    project: Option<String>,
}

pub struct CliConfig {
    pub url: String,
    pub api_key: Option<String>,
}

fn setup_logging() {
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::never(&log_dir, "mindojo-cli.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .init();

    // Keep _guard alive for program lifetime
    std::mem::forget(_guard);
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Extract => {
            setup_logging();
            if let Err(e) = extract::run().await {
                tracing::error!(error = %e, "extract hook failed");
            }
        }
        Command::Add(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut tool_args = serde_json::json!({"memory": args.memory});
                    if let Some(ref p) = args.project {
                        tool_args["project"] = serde_json::Value::String(p.clone());
                    }
                    match c.call_tool("add_memory", tool_args).await {
                        Ok(result) => println!("{result}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
        Command::Search(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut tool_args = serde_json::json!({
                        "query": args.query,
                        "limit": args.limit,
                    });
                    if let Some(ref p) = args.project {
                        tool_args["project"] = serde_json::Value::String(p.clone());
                    }
                    match c.call_tool("search_memories", tool_args).await {
                        Ok(result) => println!("{result}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
        Command::Status(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut tool_args = serde_json::json!({});
                    if let Some(ref id) = args.operation_id {
                        tool_args["operation_id"] = serde_json::Value::String(id.clone());
                    }
                    match c.call_tool("get_queue_status", tool_args).await {
                        Ok(result) => println!("{result}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
        Command::Count(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut tool_args = serde_json::json!({});
                    if let Some(ref p) = args.project {
                        tool_args["project"] = serde_json::Value::String(p.clone());
                    }
                    match c.call_tool("count_memories", tool_args).await {
                        Ok(result) => println!("{result}"),
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
    }
}
