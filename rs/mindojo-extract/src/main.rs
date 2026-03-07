//! mindojo-extract — Claude Code hook handler for SubagentStop and PreCompact events.
//!
//! Reads JSON from stdin, dispatches by `hook_event_name`, runs the memory
//! pipeline (extract -> dedup -> store), and logs results.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use mindojo_core::error::{Result as MindojoResult, ResultExt};
use serde::{Deserialize, Serialize};
use tracing::info;

use mindojo_core::config::Settings;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::llm::GenaiLlmProvider;
use mindojo_core::pipeline::{MEMORIES_TABLE, do_add_memory};
use mindojo_core::prompts::FACT_EXTRACTION_HOOK_PROMPT;
use mindojo_core::traits::{EmbeddingProvider, VectorStore};

/// Minimum message length to consider for extraction.
const MIN_MESSAGE_LENGTH: usize = 70;

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

/// Append one JSONL entry to the hook data log. Never panics.
fn log_hook_data(
    log_path: &Path,
    hook: &str,
    project: Option<&str>,
    user_id: &str,
    content: &str,
    facts: Option<&[String]>,
    decision: &str,
) {
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

    if let Ok(json) = serde_json::to_string(&entry)
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{}", json);
    }
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
            "SubagentStop: message too short, skipping"
        );
        log_hook_data(
            hook_data_log,
            "SubagentStop",
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
        user_id, "SubagentStop: running memory pipeline"
    );

    let metadata = serde_json::json!({
        "type": "lesson",
        "source": "auto-agent-stop",
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
        "SubagentStop",
        project.as_deref(),
        &user_id,
        message,
        facts.as_deref(),
        decision,
    );

    info!(decision, "SubagentStop: pipeline complete");
    Ok(())
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

    let path = PathBuf::from(&transcript_path);
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

    // Find last assistant text
    let mut last_text: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<TranscriptLine>(line)
            && let Some(text) = extract_text_from_transcript_line(&obj)
        {
            last_text = Some(text);
        }
    }

    let cwd = payload.cwd.as_deref().unwrap_or("");
    let project = if cwd.is_empty() {
        None
    } else {
        resolve_project(cwd)
    };
    let user_id = project
        .as_ref()
        .map(|p| format!("project:{}", p))
        .unwrap_or_else(|| settings.default_user_id.clone());

    let message = last_text.as_deref().unwrap_or("");

    if message.len() < MIN_MESSAGE_LENGTH {
        info!(
            len = message.len(),
            "PreCompact: last assistant message too short, skipping"
        );
        log_hook_data(
            hook_data_log,
            "PreCompact",
            project.as_deref(),
            &user_id,
            message,
            None,
            "skipped_short",
        );
        return Ok(());
    }

    info!(len = message.len(), "PreCompact: extracted message");

    let metadata = serde_json::json!({
        "type": "lesson",
        "source": "auto-pre-compact",
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
        "PreCompact",
        project.as_deref(),
        &user_id,
        message,
        facts.as_deref(),
        decision,
    );

    info!(decision, "PreCompact: pipeline complete");
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

    // The outer try-catch ensures the hook never crashes
    if let Err(e) = run(&hook_data_log).await {
        tracing::error!(error = %e, "extract_learnings hook failed");
    }
}

async fn run(hook_data_log: &Path) -> MindojoResult<()> {
    // Read JSON from stdin
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("failed to read stdin")?;

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

    let settings = Settings::load();
    let embedder = FastEmbedProvider::from_settings(&settings)?;
    let llm = GenaiLlmProvider::from_settings(&settings);
    let store = LanceDbStore::open(&settings.lancedb_path).await?;

    match payload.hook_event_name.as_str() {
        "SubagentStop" => {
            handle_subagent_stop(&payload, &settings, &embedder, &llm, &store, hook_data_log)
                .await
        }
        "PreCompact" => {
            handle_precompact(&payload, &settings, &embedder, &llm, &store, hook_data_log).await
        }
        other => {
            info!(event = other, "Hook: unhandled event, skipping");
            Ok(())
        }
    }
}
