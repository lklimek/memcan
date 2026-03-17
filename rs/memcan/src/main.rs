//! memcan — thin MCP client for MemCan server operations.
//!
//! No dependency on memcan-core (no fastembed, LanceDB, or genai).
//! Communicates with the MemCan server over HTTP via MCP protocol.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod client;
mod extract;
mod walk;

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

    /// Export a collection to JSONL.
    Export(ExportArgs),

    /// Import records from a JSONL file.
    Import(ImportArgs),

    /// Index source code files on the server.
    IndexCode(IndexCodeArgs),
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

#[derive(Parser)]
struct ExportArgs {
    /// Collection to export (memories, standards, code, todos).
    collection: String,

    /// Output file path (stdout if omitted).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// SQL filter string.
    #[arg(long)]
    filter: Option<String>,

    /// Records per page.
    #[arg(long, default_value = "1000", value_parser = parse_page_size)]
    page_size: u32,
}

#[derive(Parser)]
struct ImportArgs {
    /// JSONL file to import.
    file: PathBuf,

    /// Records per MCP call.
    #[arg(long, default_value = "50", value_parser = parse_batch_size)]
    batch_size: usize,
}

#[derive(Parser)]
struct IndexCodeArgs {
    /// Root directory to index.
    dir: PathBuf,

    /// Project name.
    #[arg(long)]
    project: String,

    /// Tech stack (auto-detected if omitted).
    #[arg(long)]
    tech_stack: Option<String>,

    /// Skip files larger than this (bytes).
    #[arg(long, default_value = "1048576")]
    max_file_size: u64,

    /// Files per MCP call.
    #[arg(long, default_value = "10", value_parser = parse_batch_size)]
    batch_size: usize,

    /// Wait for all operations to complete.
    #[arg(long)]
    wait: bool,
}

fn parse_batch_size(s: &str) -> Result<usize, String> {
    let n: usize = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if n == 0 {
        return Err("batch_size must be >= 1".to_string());
    }
    Ok(n)
}

fn parse_page_size(s: &str) -> Result<u32, String> {
    let n: u32 = s
        .parse()
        .map_err(|_| format!("'{s}' is not a valid number"))?;
    if n == 0 {
        return Err("page_size must be >= 1".to_string());
    }
    Ok(n)
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

async fn send_import_batch(client: &client::McpClient, batch: &[String]) -> (u64, u64) {
    let records = batch.join("\n");
    match client
        .call_tool("_import_records", serde_json::json!({ "records": records }))
        .await
    {
        Ok(result) => {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&result) {
                let imported = parsed.get("imported").and_then(|v| v.as_u64()).unwrap_or(0);
                let error_arr = parsed.get("errors").and_then(|v| v.as_array());
                let errors = error_arr.map(|a| a.len() as u64).unwrap_or(0);
                if let Some(arr) = error_arr {
                    for err in arr {
                        if let Some(msg) = err.as_str() {
                            eprintln!("  import error: {msg}");
                        }
                    }
                }
                (imported, errors)
            } else {
                (0, batch.len() as u64)
            }
        }
        Err(e) => {
            eprintln!("Error importing batch: {e}");
            (0, batch.len() as u64)
        }
    }
}

async fn poll_until_done(client: &client::McpClient, operation_id: &str) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        match client
            .call_tool(
                "get_queue_status",
                serde_json::json!({ "operation_id": operation_id }),
            )
            .await
        {
            Ok(result) => {
                if let Ok(status) = serde_json::from_str::<serde_json::Value>(&result) {
                    let step = status
                        .get("step")
                        .or_else(|| status.get("status"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if step == "completed" || step == "completed_degraded" || step == "failed" {
                        if step == "failed" {
                            eprintln!("Operation {operation_id} failed");
                        }
                        break;
                    }
                    eprint!("\r  {operation_id}: {step}");
                }
            }
            Err(e) => {
                eprintln!("Error polling {operation_id}: {e}");
                break;
            }
        }
    }
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
        Command::Export(args) => {
            let config = client::load_config();
            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut writer: Box<dyn Write> = match &args.output {
                        Some(path) => match std::fs::File::create(path) {
                            Ok(f) => Box::new(std::io::BufWriter::new(f)),
                            Err(e) => {
                                eprintln!("Error creating output file: {e}");
                                std::process::exit(1);
                            }
                        },
                        None => Box::new(std::io::stdout().lock()),
                    };

                    let mut offset: u32 = 0;
                    let mut total: u32 = 0;
                    loop {
                        let mut tool_args = serde_json::json!({
                            "collection": args.collection,
                            "limit": args.page_size,
                            "offset": offset,
                        });
                        if let Some(ref f) = args.filter {
                            tool_args["filter"] = serde_json::Value::String(f.clone());
                        }
                        match c.call_tool("export_collection", tool_args).await {
                            Ok(result) => {
                                let parsed: serde_json::Value = match serde_json::from_str(&result)
                                {
                                    Ok(v) => v,
                                    Err(e) => {
                                        eprintln!("Error parsing server response: {e}");
                                        std::process::exit(1);
                                    }
                                };
                                let data =
                                    parsed.get("data").and_then(|v| v.as_str()).unwrap_or("");
                                let count =
                                    parsed.get("count").and_then(|v| v.as_u64()).unwrap_or(0)
                                        as u32;

                                if !data.is_empty()
                                    && let Err(e) = writeln!(writer, "{data}")
                                {
                                    eprintln!("Error writing output: {e}");
                                    std::process::exit(1);
                                }
                                total += count;
                                if count < args.page_size {
                                    break;
                                }
                                offset += args.page_size;
                            }
                            Err(e) => {
                                eprintln!("Error: {e}");
                                std::process::exit(1);
                            }
                        }
                    }
                    eprintln!("Exported {total} records from {}", args.collection);
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
        Command::Import(args) => {
            let config = client::load_config();
            let file = match std::fs::File::open(&args.file) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Error opening file: {e}");
                    std::process::exit(1);
                }
            };
            let reader = std::io::BufReader::new(file);

            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut total_imported: u64 = 0;
                    let mut total_errors: u64 = 0;
                    let mut batch: Vec<String> = Vec::with_capacity(args.batch_size);

                    for line in reader.lines() {
                        let line = match line {
                            Ok(l) => l,
                            Err(e) => {
                                eprintln!("Error reading line: {e}");
                                total_errors += 1;
                                continue;
                            }
                        };
                        if line.trim().is_empty() {
                            continue;
                        }
                        batch.push(line);

                        if batch.len() >= args.batch_size {
                            let (imported, errors) = send_import_batch(&c, &batch).await;
                            total_imported += imported;
                            total_errors += errors;
                            batch.clear();
                        }
                    }

                    if !batch.is_empty() {
                        let (imported, errors) = send_import_batch(&c, &batch).await;
                        total_imported += imported;
                        total_errors += errors;
                    }

                    eprintln!("Imported {total_imported} records ({total_errors} errors)");
                    c.close().await;
                }
                Err(e) => eprintln!("Connection failed: {e}"),
            }
        }
        Command::IndexCode(args) => {
            let config = client::load_config();
            let opts = walk::WalkOptions {
                max_file_size: args.max_file_size,
            };
            let files = match walk::walk_directory(&args.dir, &opts) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("Error walking directory: {e}");
                    std::process::exit(1);
                }
            };

            if files.is_empty() {
                eprintln!("No supported files found in {}", args.dir.display());
                std::process::exit(1);
            }

            let tech_stack = match args.tech_stack {
                Some(ts) => ts,
                None => match walk::detect_tech_stack(&files) {
                    Ok(ts) => {
                        eprintln!("Auto-detected tech stack: {ts}");
                        ts
                    }
                    Err(e) => {
                        eprintln!("Error: {e}");
                        std::process::exit(1);
                    }
                },
            };

            let batches = walk::chunk_into_batches(files, args.batch_size);
            let total_files: usize = batches.iter().map(|b| b.len()).sum();
            eprintln!("Indexing {total_files} files in {} batches", batches.len());

            match client::McpClient::connect(&config).await {
                Ok(c) => {
                    let mut operation_ids: Vec<String> = Vec::new();

                    for (i, batch) in batches.iter().enumerate() {
                        let files_json: Vec<serde_json::Value> = batch
                            .iter()
                            .map(|f| {
                                serde_json::json!({
                                    "path": f.relative_path,
                                    "content": f.content,
                                })
                            })
                            .collect();

                        let tool_args = serde_json::json!({
                            "files": files_json,
                            "project": args.project,
                            "tech_stack": tech_stack,
                        });

                        match c.call_tool("index_code_files", tool_args).await {
                            Ok(result) => {
                                if let Ok(parsed) =
                                    serde_json::from_str::<serde_json::Value>(&result)
                                    && let Some(op_id) =
                                        parsed.get("operation_id").and_then(|v| v.as_str())
                                {
                                    operation_ids.push(op_id.to_string());
                                }
                                eprintln!(
                                    "  Batch {}/{}: {} files submitted",
                                    i + 1,
                                    batches.len(),
                                    batch.len()
                                );
                            }
                            Err(e) => {
                                eprintln!("Error submitting batch {}: {e}", i + 1);
                            }
                        }
                    }

                    if args.wait && !operation_ids.is_empty() {
                        eprintln!(
                            "Waiting for {} operations to complete...",
                            operation_ids.len()
                        );
                        for op_id in &operation_ids {
                            poll_until_done(&c, op_id).await;
                        }
                    }

                    eprintln!(
                        "Submitted {total_files} files ({} operations)",
                        operation_ids.len()
                    );
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
