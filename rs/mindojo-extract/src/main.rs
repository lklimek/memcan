//! mindojo-extract — Claude Code hook handler for SubagentStop and PreCompact events.
//!
//! Reads JSON from stdin, dispatches by `hook_event_name`, runs the memory
//! pipeline (extract -> dedup -> store), and logs results.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use mindojo_core::error::{Result as MindojoResult, ResultExt};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;
use tracing::{info, warn};

use mindojo_core::config::Settings;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::init::MindojoContext;
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::llm::GenaiLlmProvider;
use mindojo_core::pipeline::{MEMORIES_TABLE, do_add_memory};
use mindojo_core::prompts::FACT_EXTRACTION_HOOK_PROMPT;
use mindojo_core::traits::{EmbeddingProvider, VectorStore};

/// Minimum message length to consider for extraction.
const MIN_MESSAGE_LENGTH: usize = 70;

/// Maximum stdin payload size (32 MB).
const MAX_STDIN_BYTES: u64 = 32 * 1024 * 1024;

/// JSONL log entry for hook data.
#[derive(Serialize)]
struct HookDataEntry {
    ts: String,
    hook: String,
    project: Option<String>,
    user_id: String,
    content_length: usize,
    content: String,
    facts: Option<Vec<String>>,
    decision: String,
    prompt_file: String,
}

/// Hook input payload from Claude Code.
#[derive(Deserialize)]
struct HookPayload {
    #[serde(default)]
    hook_event_name: String,
    #[serde(default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    transcript_path: Option<String>,
}

/// Transcript JSONL line structure.
#[derive(Deserialize)]
struct TranscriptLine {
    #[serde(rename = "type")]
    line_type: Option<String>,
    message: Option<TranscriptMessage>,
}

#[derive(Deserialize)]
struct TranscriptMessage {
    role: Option<String>,
    content: Option<serde_json::Value>,
}

/// Extract repo name from a git remote URL (HTTPS, SSH protocol, or SSH shorthand).
fn repo_name_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // SSH shorthand: git@github.com:user/repo.git
    let path_part = if url.contains(':')
        && !url.starts_with("https://")
        && !url.starts_with("http://")
        && !url.starts_with("ssh://")
    {
        url.split_once(':').map(|(_, p)| p).unwrap_or(url)
    } else {
        url
    };

    let name = Path::new(path_part)
        .file_name()
        .map(|n: &std::ffi::OsStr| n.to_string_lossy().to_string())?;

    let name = name.strip_suffix(".git").unwrap_or(&name).to_string();

    if name.is_empty() { None } else { Some(name) }
}

/// Determine project name from cwd via git remote, with fallbacks.
fn resolve_project(cwd: &str) -> Option<String> {
    // 1. Try git remote origin URL
    if let Ok(output) = Command::new("git")
        .args(["-C", cwd, "remote", "get-url", "origin"])
        .output()
        && output.status.success()
    {
        let url = String::from_utf8_lossy(&output.stdout).to_string();
        if let Some(name) = repo_name_from_url(&url)
            && !matches!(name.as_str(), "tmp" | "temp")
        {
            return Some(name);
        }
    }

    // 2. Fallback: git toplevel basename
    if let Ok(output) = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
        && output.status.success()
    {
        let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let basename = Path::new(&toplevel)
            .file_name()
            .map(|n| n.to_string_lossy().to_string());
        if let Some(name) = basename
            && !matches!(name.as_str(), "tmp" | "temp" | "/")
            && !name.is_empty()
        {
            return Some(name);
        }
    }

    // 3. Fallback: cwd basename
    let cwd_path = Path::new(cwd);
    let resolved = cwd_path
        .canonicalize()
        .unwrap_or_else(|_| cwd_path.to_path_buf());
    if let Some(home) = dirs::home_dir()
        && resolved == home
    {
        return None;
    }
    if resolved.components().count() <= 2 {
        return None;
    }
    let basename = resolved.file_name()?.to_string_lossy().to_string();
    if matches!(basename.to_lowercase().as_str(), "tmp" | "temp") || basename.is_empty() {
        return None;
    }
    Some(basename)
}

/// Extract assistant text content from a transcript JSONL line.
fn extract_text_from_transcript_line(line_obj: &TranscriptLine) -> Option<String> {
    if line_obj.line_type.as_deref() != Some("assistant") {
        return None;
    }

    let message = line_obj.message.as_ref()?;
    if message.role.as_deref() != Some("assistant") {
        return None;
    }

    let content = message.content.as_ref()?;
    let blocks = content.as_array()?;

    let texts: Vec<String> = blocks
        .iter()
        .filter_map(|block| {
            let block_type = block.get("type")?.as_str()?;
            if block_type != "text" {
                return None;
            }
            let text = block.get("text")?.as_str()?;
            if text.is_empty() {
                return None;
            }
            Some(text.to_string())
        })
        .collect();

    if texts.is_empty() {
        None
    } else {
        Some(texts.join(""))
    }
}

/// Append one JSONL entry to the hook data log.
/// Enabled by setting `MINDOJO_DEBUG_HOOKS=true` in environment.
fn log_hook_data(
    log_path: &Path,
    hook: &str,
    project: Option<&str>,
    user_id: &str,
    content: &str,
    facts: Option<&[String]>,
    decision: &str,
) {
    if std::env::var("MINDOJO_DEBUG_HOOKS").as_deref() != Ok("true") {
        return;
    }

    let entry = HookDataEntry {
        ts: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        hook: hook.to_string(),
        project: project.map(|s| s.to_string()),
        user_id: user_id.to_string(),
        content_length: content.len(),
        content: content.to_string(),
        facts: facts.map(|f| f.to_vec()),
        decision: decision.to_string(),
        prompt_file: "fact-extraction-hook.md".to_string(),
    };

    match serde_json::to_string(&entry) {
        Ok(json) => {
            match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    if let Err(e) = writeln!(file, "{}", json) {
                        tracing::warn!(error = %e, "failed to write hook data log entry");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, path = %log_path.display(), "failed to open hook data log");
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to serialize hook data entry");
        }
    }
}

/// Validate that a path is within the user's home directory.
/// Rejects paths to sensitive system locations.
fn validate_path(path: &Path) -> MindojoResult<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home"));

    // Resolve symlinks and relative components — reject if path doesn't exist
    let resolved = path.canonicalize().map_err(|e| {
        mindojo_core::error::MindojoError::Other(format!(
            "path does not exist or is not accessible: {}: {e}",
            path.display()
        ))
    })?;

    let sensitive = ["/etc", "/proc", "/sys", "/dev", "/boot", "/root"];
    for prefix in &sensitive {
        if resolved.starts_with(prefix) {
            return Err(mindojo_core::error::MindojoError::Other(format!(
                "path in sensitive location: {}",
                resolved.display()
            )));
        }
    }

    if !resolved.starts_with(&home) && !resolved.starts_with("/tmp") {
        return Err(mindojo_core::error::MindojoError::Other(format!(
            "path outside home directory: {}",
            resolved.display()
        )));
    }

    Ok(resolved)
}

/// Shared memory pipeline: resolve project, check length, extract facts, log.
#[allow(clippy::too_many_arguments)]
async fn process_conversation(
    message: &str,
    cwd: &str,
    event_name: &str,
    source_tag: &str,
    settings: &Settings,
    embedder: &FastEmbedProvider,
    llm: &GenaiLlmProvider,
    store: &LanceDbStore,
    hook_data_log: &Path,
) -> MindojoResult<()> {
    let project = if cwd.is_empty() {
        None
    } else {
        resolve_project(cwd)
    };
    let user_id = project
        .as_ref()
        .map(|p| format!("project:{}", p))
        .unwrap_or_else(|| settings.default_user_id.clone());

    if message.len() < MIN_MESSAGE_LENGTH {
        info!(
            len = message.len(),
            event = event_name,
            "message too short, skipping"
        );
        log_hook_data(
            hook_data_log,
            event_name,
            project.as_deref(),
            &user_id,
            message,
            None,
            "skipped_short",
        );
        return Ok(());
    }

    info!(
        project = project.as_deref().unwrap_or("none"),
        user_id,
        event = event_name,
        "running memory pipeline"
    );

    let metadata = serde_json::json!({
        "type": "lesson",
        "source": source_tag,
    });

    store
        .ensure_table(MEMORIES_TABLE, embedder.dimensions())
        .await?;

    let facts = do_add_memory(
        message,
        &user_id,
        &metadata,
        settings.distill_memories,
        MEMORIES_TABLE,
        store,
        embedder,
        llm,
        &settings.llm_model,
        Some(FACT_EXTRACTION_HOOK_PROMPT),
    )
    .await?;

    let decision = match &facts {
        None => "error",
        Some(f) if f.is_empty() => "rejected",
        Some(_) => "kept",
    };

    log_hook_data(
        hook_data_log,
        event_name,
        project.as_deref(),
        &user_id,
        message,
        facts.as_deref(),
        decision,
    );

    info!(decision, event = event_name, "pipeline complete");
    Ok(())
}

/// Handle SubagentStop event.
async fn handle_subagent_stop(
    payload: &HookPayload,
    settings: &Settings,
    embedder: &FastEmbedProvider,
    llm: &GenaiLlmProvider,
    store: &LanceDbStore,
    hook_data_log: &Path,
) -> MindojoResult<()> {
    let message = payload.last_assistant_message.as_deref().unwrap_or("");
    let cwd = payload.cwd.as_deref().unwrap_or("");

    if !cwd.is_empty() {
        validate_path(Path::new(cwd))?;
    }

    process_conversation(
        message,
        cwd,
        "SubagentStop",
        "auto-agent-stop",
        settings,
        embedder,
        llm,
        store,
        hook_data_log,
    )
    .await
}

fn noise_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)^\s*(done|committed|clean|all\s+(tests?\s+)?pass(ing|ed)?|no\s+(issues|errors|warnings))\s*\.?\s*$").unwrap(),
            Regex::new(r"(?i)^[\w./-]+\.(rs|py|go|js|ts|tsx|toml|yaml|yml|json|md|txt|lock|sh|css|html)\s*$").unwrap(),
            Regex::new(r"(?i)^(refs/heads/|origin/)?[\w./-]+\s*$").unwrap(),
            Regex::new(r"(?i)^[0-9a-f]{7,40}\s*$").unwrap(),
            Regex::new(r"^(\u{2713}|\u{2717}|PASS|FAIL|ok)\b").unwrap(),
        ]
    })
}

fn filter_messages(texts: Vec<String>) -> Vec<String> {
    let patterns = noise_patterns();
    texts
        .into_iter()
        .filter(|t| {
            if t.len() < MIN_MESSAGE_LENGTH {
                return false;
            }
            !patterns.iter().any(|re| re.is_match(t))
        })
        .collect()
}

fn chunk_messages(messages: &[String], max_chars: usize) -> Vec<String> {
    let separator = "\n---\n";
    let sep_len = separator.len();
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();

    for msg in messages {
        let addition = if current.is_empty() {
            msg.len()
        } else {
            sep_len + msg.len()
        };

        if !current.is_empty() && current.len() + addition > max_chars {
            chunks.push(std::mem::take(&mut current));
        }

        if current.is_empty() {
            current.push_str(msg);
        } else {
            current.push_str(separator);
            current.push_str(msg);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.len() > 1 {
        let total: usize = messages.iter().map(|m| m.len()).sum();
        warn!(
            total_chars = total,
            budget = max_chars,
            chunks = chunks.len(),
            "transcript exceeds context budget, splitting into chunks"
        );
    }

    chunks
}

/// Handle PreCompact event.
async fn handle_precompact(
    payload: &HookPayload,
    settings: &Settings,
    embedder: &FastEmbedProvider,
    llm: &GenaiLlmProvider,
    store: &LanceDbStore,
    hook_data_log: &Path,
) -> MindojoResult<()> {
    let transcript_path = match &payload.transcript_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            info!("PreCompact: no transcript_path, skipping");
            return Ok(());
        }
    };

    let path = validate_path(Path::new(&transcript_path))?;

    if !path.is_file() {
        info!(path = %transcript_path, "PreCompact: transcript not found");
        return Ok(());
    }

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read transcript: {}", transcript_path))?;

    info!(
        path = %transcript_path,
        lines = raw.lines().count(),
        "PreCompact: reading transcript"
    );

    let mut all_texts: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<TranscriptLine>(line)
            && let Some(text) = extract_text_from_transcript_line(&obj)
        {
            all_texts.push(text);
        }
    }

    let filtered = filter_messages(all_texts);
    if filtered.is_empty() {
        info!("PreCompact: no substantive assistant messages after filtering");
        return Ok(());
    }

    let cwd = payload.cwd.as_deref().unwrap_or("");
    if !cwd.is_empty() {
        validate_path(Path::new(cwd))?;
    }

    let prompt_overhead = 2000;
    let max_chars = (settings.context_window * 4 * 80 / 100).saturating_sub(prompt_overhead);
    let chunks = chunk_messages(&filtered, max_chars);

    if chunks.len() > 1 {
        info!(
            chunk_count = chunks.len(),
            "PreCompact: processing multiple chunks"
        );
    }

    for chunk in &chunks {
        process_conversation(
            chunk,
            cwd,
            "PreCompact",
            "auto-pre-compact",
            settings,
            embedder,
            llm,
            store,
            hook_data_log,
        )
        .await?;
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    // Set up logging to file
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude")
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::never(&log_dir, "mindojo-hooks.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .init();

    let hook_data_log = log_dir.join("mindojo-hook-data.jsonl");

    if let Err(e) = run(&hook_data_log).await {
        tracing::error!(error = %e, "extract_learnings hook failed");
    }
}

async fn run(hook_data_log: &Path) -> MindojoResult<()> {
    // Read JSON from stdin asynchronously with a 32 MB limit
    let mut raw = String::new();
    let mut stdin = tokio::io::stdin().take(MAX_STDIN_BYTES);
    let bytes_read = stdin.read_to_string(&mut raw).await.map_err(|e| {
        mindojo_core::error::MindojoError::Other(format!("failed to read stdin: {e}"))
    })?;

    if bytes_read as u64 >= MAX_STDIN_BYTES {
        return Err(mindojo_core::error::MindojoError::Other(format!(
            "stdin payload exceeds {MAX_STDIN_BYTES} byte limit"
        )));
    }

    info!(bytes = raw.len(), "Hook invoked");

    if raw.trim().is_empty() {
        info!("Hook: no input, exiting");
        return Ok(());
    }

    let payload: HookPayload =
        serde_json::from_str(&raw).context("failed to parse hook payload")?;

    info!(
        event = %payload.hook_event_name,
        "Hook: dispatching event"
    );

    let ctx = MindojoContext::init().await?;
    let llm = GenaiLlmProvider::from_settings(&ctx.settings);

    match payload.hook_event_name.as_str() {
        "SubagentStop" => {
            handle_subagent_stop(
                &payload,
                &ctx.settings,
                &ctx.embedder,
                &llm,
                &ctx.store,
                hook_data_log,
            )
            .await
        }
        "PreCompact" => {
            handle_precompact(
                &payload,
                &ctx.settings,
                &ctx.embedder,
                &llm,
                &ctx.store,
                hook_data_log,
            )
            .await
        }
        other => {
            info!(event = other, "Hook: unhandled event, skipping");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_messages_drops_short() {
        let keeper =
            "This message has enough length and spaces to survive all noise filters easily."
                .to_string();
        assert!(keeper.len() >= MIN_MESSAGE_LENGTH);
        let msgs = vec!["short".to_string(), "a".repeat(69), keeper.clone()];
        let result = filter_messages(msgs);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], keeper);
    }

    #[test]
    fn test_filter_messages_drops_status() {
        let patterns = noise_patterns();
        let status_msgs = [
            "done",
            "Done.",
            "committed",
            "clean",
            "all tests passing",
            "All tests passed.",
            "no issues",
            "No errors",
            "no warnings",
        ];
        for msg in &status_msgs {
            assert!(
                patterns.iter().any(|re| re.is_match(msg)),
                "expected noise match for: {msg}"
            );
        }
    }

    #[test]
    fn test_filter_messages_keeps_substantive() {
        let msgs = vec![
            "This is a substantive message that contains real content and should survive filtering without issues.".to_string(),
            "The architecture uses a layered approach with clear separation of concerns between the transport and storage layers.".to_string(),
        ];
        let result = filter_messages(msgs.clone());
        assert_eq!(result, msgs);
    }

    #[test]
    fn test_chunk_messages_single_chunk() {
        let msgs = vec!["hello world".repeat(5), "another message".repeat(3)];
        let chunks = chunk_messages(&msgs, 10000);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].contains("\n---\n"));
    }

    #[test]
    fn test_chunk_messages_splits() {
        let msgs = vec!["a".repeat(100), "b".repeat(100), "c".repeat(100)];
        let chunks = chunk_messages(&msgs, 150);
        assert!(
            chunks.len() > 1,
            "expected multiple chunks, got {}",
            chunks.len()
        );
    }

    #[test]
    fn test_chunk_messages_oversized_single() {
        let big = "x".repeat(500);
        let msgs = vec![big.clone()];
        let chunks = chunk_messages(&msgs, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], big);
    }

    #[test]
    fn test_chunk_messages_join_format() {
        let msgs = vec!["aaa".to_string(), "bbb".to_string()];
        let chunks = chunk_messages(&msgs, 100000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "aaa\n---\nbbb");
    }
}
