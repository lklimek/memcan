//! memcan — thin MCP client for MemCan server operations.
//!
//! No dependency on memcan-core (no fastembed, LanceDB, or genai).
//! Communicates with the MemCan server over HTTP via MCP protocol.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod client;
mod extract;

#[derive(Parser)]
#[command(
    name = "memcan",
    about = "MemCan thin CLI client",
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

    /// Index a markdown standards document on the server.
    IndexStandards(IndexStandardsArgs),
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

#[derive(Parser)]
struct IndexStandardsArgs {
    /// Markdown file to index (required unless --drop).
    file: Option<PathBuf>,

    /// Standard identifier.
    #[arg(long)]
    standard_id: String,

    /// Type of standard (security, coding, cve, guideline). Required unless --drop.
    #[arg(long)]
    standard_type: Option<String>,

    /// Standard version.
    #[arg(long)]
    version: Option<String>,

    /// Language code.
    #[arg(long)]
    lang: Option<String>,

    /// Source URL.
    #[arg(long)]
    url: Option<String>,

    /// Drop all indexed data for --standard-id instead of indexing.
    #[arg(long)]
    drop: bool,

    /// Wait for indexing to complete (poll get_queue_status).
    #[arg(long)]
    wait: bool,
}

pub struct CliConfig {
    pub url: String,
    pub api_key: Option<String>,
}

fn setup_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::never(&log_dir, "memcan.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .init();

    guard
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Extract => {
            let _log_guard = setup_logging();
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
        Command::IndexStandards(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    if args.drop {
                        match c
                            .call_tool(
                                "drop_indexed_standards",
                                serde_json::json!({ "standard_id": args.standard_id }),
                            )
                            .await
                        {
                            Ok(result) => println!("{result}"),
                            Err(e) => eprintln!("Error: {e}"),
                        }
                        c.close().await;
                        return;
                    }

                    let file = match args.file.as_ref() {
                        Some(f) => f,
                        None => {
                            eprintln!("Error: file is required unless --drop");
                            std::process::exit(1);
                        }
                    };
                    let content = match std::fs::read_to_string(file) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("Error reading file: {e}");
                            std::process::exit(1);
                        }
                    };
                    let standard_type = match args.standard_type.as_deref() {
                        Some(t) => t,
                        None => {
                            eprintln!("Error: --standard-type is required for indexing");
                            std::process::exit(1);
                        }
                    };

                    let mut tool_args = serde_json::json!({
                        "content": content,
                        "standard_id": args.standard_id,
                        "standard_type": standard_type,
                    });
                    if let Some(ref v) = args.version {
                        tool_args["version"] = serde_json::json!(v);
                    }
                    if let Some(ref l) = args.lang {
                        tool_args["lang"] = serde_json::json!(l);
                    }
                    if let Some(ref u) = args.url {
                        tool_args["url"] = serde_json::json!(u);
                    }

                    match c.call_tool("index_standards", tool_args).await {
                        Ok(result) => {
                            if args.wait {
                                if let Ok(parsed) =
                                    serde_json::from_str::<serde_json::Value>(&result)
                                {
                                    if let Some(op_id) =
                                        parsed.get("operation_id").and_then(|v| v.as_str())
                                    {
                                        loop {
                                            tokio::time::sleep(std::time::Duration::from_secs(2))
                                                .await;
                                            match c
                                                .call_tool(
                                                    "get_queue_status",
                                                    serde_json::json!({
                                                        "operation_id": op_id,
                                                    }),
                                                )
                                                .await
                                            {
                                                Ok(status_result) => {
                                                    if let Ok(status) =
                                                        serde_json::from_str::<serde_json::Value>(
                                                            &status_result,
                                                        )
                                                    {
                                                        let step = status
                                                            .get("step")
                                                            .or_else(|| status.get("status"))
                                                            .and_then(|v| v.as_str())
                                                            .unwrap_or("");
                                                        if step == "completed"
                                                            || step == "completed_degraded"
                                                            || step == "failed"
                                                        {
                                                            println!(
                                                                "{}",
                                                                serde_json::to_string_pretty(
                                                                    &status
                                                                )
                                                                .unwrap_or(status_result)
                                                            );
                                                            if step == "failed" {
                                                                c.close().await;
                                                                std::process::exit(1);
                                                            }
                                                            break;
                                                        }
                                                        eprint!("\r{step}");
                                                    }
                                                }
                                                Err(e) => {
                                                    eprintln!("Error polling status: {e}");
                                                    break;
                                                }
                                            }
                                        }
                                    } else {
                                        println!("{result}");
                                    }
                                } else {
                                    println!("{result}");
                                }
                            } else {
                                println!("{result}");
                            }
                        }
                        Err(e) => eprintln!("Error: {e}"),
                    }
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
    }
}
