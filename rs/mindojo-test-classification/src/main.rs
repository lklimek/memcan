//! mindojo-test-classification — Replay hook data log for prompt tuning.
//!
//! Reads a JSONL hook data log, re-runs fact extraction with a specified
//! prompt/model, and calculates TP/FP/TN/FN/accuracy/precision/recall.

use std::path::PathBuf;

use chrono::Utc;
use clap::Parser;
use mindojo_core::config::Settings;
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use mindojo_core::llm::GenaiLlmProvider;
use mindojo_core::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use serde::Deserialize;

#[derive(Parser)]
#[command(name = "mindojo-test-classification")]
#[command(about = "Replay hook data log with a different prompt/model for prompt tuning")]
struct Cli {
    /// Path to fact-extraction prompt .md file
    #[arg(long)]
    prompt: PathBuf,

    /// LLM model name (genai format, e.g. ollama::qwen3.5:4b)
    #[arg(long)]
    model: String,

    /// Path to JSONL data file (default: ~/.claude/logs/mindojo-hook-data.jsonl)
    #[arg(long)]
    data: Option<PathBuf>,
}

fn default_data_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".claude")
        .join("logs")
        .join("mindojo-hook-data.jsonl")
}

/// A single entry from the hook data JSONL log.
#[derive(Deserialize)]
struct HookDataEntry {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    content: String,
}

/// Parsed response from the fact extraction LLM call.
#[derive(Deserialize)]
struct FactsResponse {
    #[serde(default)]
    facts: Vec<String>,
}

/// Send content to the LLM and parse facts. Returns None on failure.
async fn call_llm(
    llm: &dyn LlmProvider,
    model: &str,
    system_prompt: &str,
    content: &str,
    max_attempts: u32,
) -> Option<Vec<String>> {
    let messages = vec![
        LlmMessage {
            role: Role::System,
            content: system_prompt.to_string(),
        },
        LlmMessage {
            role: Role::User,
            content: content.to_string(),
        },
    ];

    let options = Some(LlmOptions {
        format_json: true,
        max_tokens: Some(1024),
        ..Default::default()
    });

    for attempt in 1..=max_attempts {
        match llm.chat(model, &messages, options.clone()).await {
            Ok(text) => match serde_json::from_str::<FactsResponse>(&text) {
                Ok(parsed) => return Some(parsed.facts),
                Err(e) => {
                    println!("  Parse error: {}", e);
                    return None;
                }
            },
            Err(e) => {
                if attempt < max_attempts {
                    println!(
                        "  Attempt {}/{} failed ({}), retrying in 5s...",
                        attempt, max_attempts, e
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                } else {
                    println!("  All {} attempts failed: {}", max_attempts, e);
                    return None;
                }
            }
        }
    }

    None
}

#[tokio::main]
async fn main() -> MindojoResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();

    // Load settings via Settings::load() (handles .env files via dotenvy)
    let settings = Settings::load()?;
    let llm = GenaiLlmProvider::from_settings(&settings);

    if !cli.prompt.is_file() {
        return Err(MindojoError::Other(format!(
            "Prompt file not found: {}",
            cli.prompt.display()
        )));
    }

    let data_path = cli.data.unwrap_or_else(default_data_path);

    if !data_path.is_file() {
        return Err(MindojoError::Other(format!(
            "Data file not found: {}",
            data_path.display()
        )));
    }

    let prompt_template = std::fs::read_to_string(&cli.prompt)
        .with_context(|| format!("failed to read prompt: {}", cli.prompt.display()))?;

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let system_prompt = prompt_template.replace("$today", &today);

    // Load testable entries
    let data_raw = std::fs::read_to_string(&data_path)
        .with_context(|| format!("failed to read data: {}", data_path.display()))?;

    let mut entries: Vec<HookDataEntry> = Vec::new();
    for line in data_raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<HookDataEntry>(line) {
            Ok(entry) => {
                if entry.decision == "kept" || entry.decision == "rejected" {
                    entries.push(entry);
                }
            }
            Err(_) => continue,
        }
    }

    if entries.is_empty() {
        println!("No testable entries found (need 'kept' or 'rejected' decisions)");
        return Ok(());
    }

    println!(
        "Loaded {} testable entries from {}",
        entries.len(),
        data_path.display()
    );
    println!("Model: {}", cli.model);
    println!("Prompt: {}", cli.prompt.display());
    println!();

    let mut tp: u32 = 0;
    let mut fp: u32 = 0;
    let mut tn: u32 = 0;
    let mut r#fn: u32 = 0;

    for (i, entry) in entries.iter().enumerate() {
        let expect_facts = entry.decision == "kept";

        let new_facts = call_llm(&llm, &cli.model, &system_prompt, &entry.content, 3).await;

        let (status, label) = match &new_facts {
            None => ("ERROR", "---"),
            Some(facts) => {
                let got_facts = !facts.is_empty();
                if expect_facts && got_facts {
                    tp += 1;
                    ("TP", "KEPT")
                } else if expect_facts && !got_facts {
                    r#fn += 1;
                    ("FN", "KEPT")
                } else if !expect_facts && !got_facts {
                    tn += 1;
                    ("TN", "REJECTED")
                } else {
                    fp += 1;
                    ("FP", "REJECTED")
                }
            }
        };

        let prefix: String = entry
            .content
            .chars()
            .take(60)
            .collect::<String>()
            .replace('\n', " ");
        let n_facts = new_facts
            .as_ref()
            .map(|f| f.len().to_string())
            .unwrap_or_else(|| "?".to_string());

        println!(
            "[{}/{}] {} {} facts={} | {}...",
            i + 1,
            entries.len(),
            status,
            label,
            n_facts,
            prefix
        );
    }

    println!();
    println!("{}", "=".repeat(60));

    let total = tp + fp + tn + r#fn;
    let accuracy = if total > 0 {
        (tp + tn) as f64 / total as f64 * 100.0
    } else {
        0.0
    };
    let precision = if (tp + fp) > 0 {
        tp as f64 / (tp + fp) as f64 * 100.0
    } else {
        0.0
    };
    let recall = if (tp + r#fn) > 0 {
        tp as f64 / (tp + r#fn) as f64 * 100.0
    } else {
        0.0
    };

    println!("Total entries:  {}", total);
    println!("True Positive:  {}", tp);
    println!("False Positive: {}", fp);
    println!("True Negative:  {}", tn);
    println!("False Negative: {}", r#fn);
    println!("Accuracy:       {:.1}%", accuracy);
    println!("Precision:      {:.1}%", precision);
    println!("Recall:         {:.1}%", recall);

    Ok(())
}
