//! memcan — thin MCP client for MemCan server operations.
//!
//! No dependency on memcan-core (no fastembed, LanceDB, or genai).
//! Communicates with the MemCan server over HTTP via MCP protocol.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

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

/// Pending-task threshold below which the CLI submits the next batch.
///
/// The server caps in-flight async operations at 20 (`MAX_PENDING_TASKS`).
/// We pace below that with a margin so a poll/submit race rarely hits the cap;
/// `submit_batch_with_retry` covers the residual race.
const PACING_THRESHOLD: u64 = 16;

/// Backoff ceiling and total budget for retrying a busy-rejected batch.
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const RETRY_BUDGET: Duration = Duration::from_secs(900);

/// One MCP tool call. Abstracts [`client::McpClient`] so the pacing/retry
/// helpers can be unit-tested against a mock.
trait ToolCaller {
    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String, String>;
}

impl ToolCaller for client::McpClient {
    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> Result<String, String> {
        client::McpClient::call_tool(self, name, arguments)
            .await
            .map_err(|e| e.to_string())
    }
}

/// True if the error is the server's "queue full" rejection (vs. a real failure).
fn is_server_busy(err: &str) -> bool {
    err.contains("Server busy") && err.contains("operations pending")
}

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, then capped at [`BACKOFF_MAX`].
fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(BACKOFF_MAX)
}

/// Reads the global `pending_tasks` count from a no-arg `get_queue_status` call.
/// Returns `None` if the server is unreachable or the field is absent.
async fn pending_tasks(client: &impl ToolCaller) -> Option<u64> {
    let result = client
        .call_tool("get_queue_status", serde_json::json!({}))
        .await
        .ok()?;
    serde_json::from_str::<serde_json::Value>(&result)
        .ok()?
        .get("pending_tasks")
        .and_then(|v| v.as_u64())
}

/// Blocks until the server's pending-task count drops below [`PACING_THRESHOLD`].
///
/// Best-effort: if the count can't be read it returns immediately and lets
/// `submit_batch_with_retry` handle any resulting busy rejection.
async fn wait_for_capacity(client: &impl ToolCaller) {
    loop {
        match pending_tasks(client).await {
            Some(n) if n >= PACING_THRESHOLD => {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            _ => return,
        }
    }
}

/// Submits one `index_code_files` batch, retrying on server-busy until accepted.
///
/// Returns the `operation_id` on success. Hard errors (non-busy) and an
/// exhausted retry budget both return `Err` so the caller can fail loudly —
/// a batch is never silently dropped.
async fn submit_batch_with_retry(
    client: &impl ToolCaller,
    tool_args: &serde_json::Value,
    batch_label: &str,
) -> Result<String, String> {
    let start = tokio::time::Instant::now();
    let mut attempt: u32 = 0;
    loop {
        match client
            .call_tool("index_code_files", tool_args.clone())
            .await
        {
            Ok(result) => {
                let op_id = serde_json::from_str::<serde_json::Value>(&result)
                    .ok()
                    .and_then(|v| {
                        v.get("operation_id")
                            .and_then(|o| o.as_str())
                            .map(str::to_string)
                    });
                return op_id.ok_or_else(|| {
                    format!("{batch_label}: accepted but server returned no operation_id")
                });
            }
            Err(msg) => {
                if !is_server_busy(&msg) {
                    return Err(format!("{batch_label}: {msg}"));
                }
                if start.elapsed() >= RETRY_BUDGET {
                    return Err(format!(
                        "{batch_label}: still rejected after {}s of retries (server busy); aborting to avoid dropping it",
                        RETRY_BUDGET.as_secs()
                    ));
                }
                let delay = backoff_delay(attempt);
                eprintln!(
                    "  {batch_label}: server busy, retrying in {}s",
                    delay.as_secs()
                );
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
            }
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
                tech_stack: args.tech_stack.as_deref(),
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
                    let total_batches = batches.len();

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

                        let batch_label = format!("Batch {}/{}", i + 1, total_batches);
                        wait_for_capacity(&c).await;
                        match submit_batch_with_retry(&c, &tool_args, &batch_label).await {
                            Ok(op_id) => {
                                operation_ids.push(op_id);
                                eprintln!("  {batch_label}: {} files submitted", batch.len());
                            }
                            Err(e) => {
                                eprintln!("Error: {e}");
                                eprintln!(
                                    "Aborting: {} of {} batches submitted before failure. \
                                     No batch was silently dropped; re-run to finish (indexing is idempotent).",
                                    i, total_batches
                                );
                                c.close().await;
                                std::process::exit(1);
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
                Err(e) => {
                    eprintln!("Connection failed: {e}");
                    std::process::exit(1);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_server_busy_detects_rejection() {
        assert!(is_server_busy(
            "Mcp error: -32603: Server busy: 20 operations pending (max 20). Try again later."
        ));
    }

    #[test]
    fn is_server_busy_ignores_other_errors() {
        assert!(!is_server_busy(
            "Mcp error: -32603: embedding model not found"
        ));
        assert!(!is_server_busy("Connection refused"));
        assert!(!is_server_busy(""));
    }

    #[test]
    fn backoff_delay_is_exponential_then_capped() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        assert_eq!(backoff_delay(5), BACKOFF_MAX);
    }

    #[test]
    fn backoff_delay_never_exceeds_cap_for_large_attempts() {
        assert_eq!(backoff_delay(63), BACKOFF_MAX);
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_MAX);
    }

    // Server's MAX_PENDING_TASKS is 20; keep a margin so the poll/submit race
    // rarely trips the cap. Compile-time invariant.
    const _: () = assert!(PACING_THRESHOLD < 20);

    use std::cell::RefCell;
    use std::collections::VecDeque;

    const BUSY: &str = "Mcp error: -32603: Server busy: 20 operations pending (max 20).";

    /// Replays a scripted queue of `call_tool` results, recording how many calls
    /// were made. `None` in the queue means "repeat the last scripted result
    /// forever" (used to simulate a server that stays busy).
    struct MockCaller {
        responses: RefCell<VecDeque<Result<String, String>>>,
        sticky: RefCell<Option<Result<String, String>>>,
        calls: RefCell<usize>,
    }

    impl MockCaller {
        fn scripted(responses: Vec<Result<String, String>>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                sticky: RefCell::new(None),
                calls: RefCell::new(0),
            }
        }

        fn always(result: Result<String, String>) -> Self {
            Self {
                responses: RefCell::new(VecDeque::new()),
                sticky: RefCell::new(Some(result)),
                calls: RefCell::new(0),
            }
        }

        fn call_count(&self) -> usize {
            *self.calls.borrow()
        }
    }

    impl ToolCaller for MockCaller {
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: serde_json::Value,
        ) -> Result<String, String> {
            *self.calls.borrow_mut() += 1;
            if let Some(next) = self.responses.borrow_mut().pop_front() {
                return next;
            }
            self.sticky
                .borrow()
                .clone()
                .unwrap_or_else(|| Err("no scripted response".into()))
        }
    }

    fn ok_op(id: &str) -> Result<String, String> {
        Ok(format!(r#"{{"operation_id":"{id}"}}"#))
    }

    #[tokio::test(start_paused = true)]
    async fn submit_retries_on_busy_then_succeeds() {
        let caller =
            MockCaller::scripted(vec![Err(BUSY.into()), Err(BUSY.into()), ok_op("op-123")]);
        let args = serde_json::json!({});
        let result = submit_batch_with_retry(&caller, &args, "Batch 1/5").await;
        assert_eq!(result.unwrap(), "op-123");
        assert_eq!(caller.call_count(), 3, "two busy retries then success");
    }

    #[tokio::test(start_paused = true)]
    async fn submit_returns_loud_err_on_non_busy_error() {
        let caller = MockCaller::scripted(vec![Err("embedding model not found".into())]);
        let args = serde_json::json!({});
        let err = submit_batch_with_retry(&caller, &args, "Batch 2/5")
            .await
            .unwrap_err();
        assert!(err.contains("Batch 2/5"), "names the batch: {err}");
        assert!(err.contains("embedding model not found"));
        assert_eq!(caller.call_count(), 1, "non-busy errors are not retried");
    }

    #[tokio::test(start_paused = true)]
    async fn submit_fails_loudly_when_budget_exhausted() {
        let caller = MockCaller::always(Err(BUSY.into()));
        let args = serde_json::json!({});
        let err = submit_batch_with_retry(&caller, &args, "Batch 3/5")
            .await
            .unwrap_err();
        assert!(err.contains("Batch 3/5"), "names the batch: {err}");
        assert!(err.contains("server busy"));
        assert!(caller.call_count() > 1, "retried before giving up");
    }

    #[tokio::test(start_paused = true)]
    async fn submit_errors_when_no_operation_id() {
        let caller = MockCaller::scripted(vec![Ok(r#"{"status":"queued"}"#.into())]);
        let args = serde_json::json!({});
        let err = submit_batch_with_retry(&caller, &args, "Batch 4/5")
            .await
            .unwrap_err();
        assert!(err.contains("no operation_id"), "got: {err}");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_capacity_returns_when_below_threshold() {
        let caller = MockCaller::scripted(vec![
            Ok(r#"{"pending_tasks":18}"#.into()),
            Ok(r#"{"pending_tasks":5}"#.into()),
        ]);
        wait_for_capacity(&caller).await;
        assert_eq!(caller.call_count(), 2, "waited once, then proceeded");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_capacity_returns_immediately_on_error() {
        let caller = MockCaller::always(Err("connection refused".into()));
        wait_for_capacity(&caller).await;
        assert_eq!(caller.call_count(), 1, "unreadable count must not hang");
    }

    #[tokio::test(start_paused = true)]
    async fn wait_for_capacity_returns_on_missing_field() {
        let caller = MockCaller::always(Ok(r#"{"operations":[]}"#.into()));
        wait_for_capacity(&caller).await;
        assert_eq!(
            caller.call_count(),
            1,
            "missing pending_tasks must not hang"
        );
    }
}
