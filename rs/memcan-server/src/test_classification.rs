//! Replay hook data log for prompt tuning (moved from `memcan-test-classification`).

use chrono::Utc;
use serde::Deserialize;

use memcan_core::config::Settings;
use memcan_core::error::{MemcanError, Result as MemcanResult, ResultExt};
use memcan_core::llm::GenaiLlmProvider;
use memcan_core::traits::{LlmMessage, LlmOptions, LlmProvider, Role};

use crate::TestClassificationArgs;

fn default_data_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".claude")
        .join("logs")
        .join("memcan-hook-data.jsonl")
}

#[derive(Deserialize)]
struct HookDataEntry {
    #[serde(default)]
    decision: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct FactsResponse {
    #[serde(default)]
    facts: Vec<String>,
}

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

pub async fn run(args: &TestClassificationArgs) -> MemcanResult<()> {
    let settings = Settings::load()?;
    settings.ensure_log_dir()?;
    let llm = GenaiLlmProvider::from_settings(&settings);

    if !args.prompt.is_file() {
        return Err(MemcanError::Other(format!(
            "Prompt file not found: {}",
            args.prompt.display()
        )));
    }

    let data_path = args.data.clone().unwrap_or_else(default_data_path);

    if !data_path.is_file() {
        return Err(MemcanError::Other(format!(
            "Data file not found: {}",
            data_path.display()
        )));
    }

    let prompt_template = std::fs::read_to_string(&args.prompt)
        .with_context(|| format!("failed to read prompt: {}", args.prompt.display()))?;

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let system_prompt = prompt_template.replace("$today", &today);

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
    println!("Model: {}", args.model);
    println!("Prompt: {}", args.prompt.display());
    println!();

    let mut tp: u32 = 0;
    let mut fp: u32 = 0;
    let mut tn: u32 = 0;
    let mut r#fn: u32 = 0;

    for (i, entry) in entries.iter().enumerate() {
        let expect_facts = entry.decision == "kept";
        let new_facts = call_llm(&llm, &args.model, &system_prompt, &entry.content, 3).await;

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
