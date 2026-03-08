//! Hook handler — reads stdin, filters noise, sends to MemCan server via MCP.
//!
//! Ported from `memcan-extract`: keeps the lightweight noise filtering and
//! chunking logic but replaces the heavy pipeline with HTTP calls.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use regex::Regex;
use serde::Deserialize;
use tokio::io::AsyncReadExt;
use tracing::{info, warn};

use crate::client::{McpClient, load_config};

const MIN_MESSAGE_LENGTH: usize = 70;
const MAX_STDIN_BYTES: u64 = 32 * 1024 * 1024;

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

fn repo_name_from_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

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

fn resolve_project(cwd: &str) -> Option<String> {
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

fn validate_path(path: &Path) -> Result<PathBuf, String> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home"));
    let resolved = path.canonicalize().map_err(|e| {
        format!(
            "path does not exist or is not accessible: {}: {e}",
            path.display()
        )
    })?;

    let sensitive = ["/etc", "/proc", "/sys", "/dev", "/boot", "/root"];
    for prefix in &sensitive {
        if resolved.starts_with(prefix) {
            return Err(format!(
                "path in sensitive location: {}",
                resolved.display()
            ));
        }
    }

    if !resolved.starts_with(&home) && !resolved.starts_with("/tmp") {
        return Err(format!(
            "path outside home directory: {}",
            resolved.display()
        ));
    }

    Ok(resolved)
}

async fn send_to_server(
    client: &McpClient,
    message: &str,
    project: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut args = serde_json::json!({
        "memory": message,
    });
    if let Some(p) = project {
        args["project"] = serde_json::Value::String(p.to_string());
    }
    args["metadata"] = serde_json::json!({"type": "lesson", "source": "auto-hook"});
    client.call_tool("add_memory", args).await?;
    Ok(())
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config();
    let mut raw = String::new();
    let mut stdin = tokio::io::stdin().take(MAX_STDIN_BYTES);
    let bytes_read = stdin.read_to_string(&mut raw).await?;

    if bytes_read as u64 >= MAX_STDIN_BYTES {
        return Err(format!("stdin payload exceeds {MAX_STDIN_BYTES} byte limit").into());
    }

    info!(bytes = raw.len(), "Hook invoked");

    if raw.trim().is_empty() {
        info!("Hook: no input, exiting");
        return Ok(());
    }

    let payload: HookPayload = serde_json::from_str(&raw)?;
    info!(event = %payload.hook_event_name, "Hook: dispatching event");

    match payload.hook_event_name.as_str() {
        "SubagentStop" => {
            let message = payload.last_assistant_message.as_deref().unwrap_or("");
            let cwd = payload.cwd.as_deref().unwrap_or("");

            if !cwd.is_empty() {
                validate_path(Path::new(cwd))
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            }

            if message.len() < MIN_MESSAGE_LENGTH {
                info!(len = message.len(), "message too short, skipping");
                return Ok(());
            }

            let project = if cwd.is_empty() {
                None
            } else {
                resolve_project(cwd)
            };

            let client = McpClient::connect(&config).await?;
            send_to_server(&client, message, project.as_deref()).await?;
            client.close().await;
        }
        "PreCompact" => {
            let transcript_path = match &payload.transcript_path {
                Some(p) if !p.is_empty() => p.clone(),
                _ => {
                    info!("PreCompact: no transcript_path, skipping");
                    return Ok(());
                }
            };

            let path = validate_path(Path::new(&transcript_path))
                .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

            if !path.is_file() {
                info!(path = %transcript_path, "PreCompact: transcript not found");
                return Ok(());
            }

            let raw_transcript = std::fs::read_to_string(&path)?;
            info!(
                path = %transcript_path,
                lines = raw_transcript.lines().count(),
                "PreCompact: reading transcript"
            );

            let mut all_texts: Vec<String> = Vec::new();
            for line in raw_transcript.lines() {
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
                validate_path(Path::new(cwd))
                    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            }

            let max_chars = 400_000;
            let chunks = chunk_messages(&filtered, max_chars);

            let project = if cwd.is_empty() {
                None
            } else {
                resolve_project(cwd)
            };

            let client = McpClient::connect(&config).await?;
            for chunk in &chunks {
                send_to_server(&client, chunk, project.as_deref()).await?;
            }
            client.close().await;
        }
        other => {
            info!(event = other, "Hook: unhandled event, skipping");
        }
    }

    Ok(())
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
    fn test_repo_name_from_url() {
        assert_eq!(
            repo_name_from_url("git@github.com:user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            repo_name_from_url("https://github.com/user/repo.git"),
            Some("repo".to_string())
        );
        assert_eq!(
            repo_name_from_url("https://github.com/user/repo"),
            Some("repo".to_string())
        );
    }
}
