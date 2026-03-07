//! mindojo-import-triaged — Import triaged findings into vector store.
//!
//! Reads a triage-annotated report JSON, filters for findings with `action == "fix"`,
//! embeds and upserts them into the memories table.

use std::path::PathBuf;

use chrono::Utc;
use clap::Parser;
use md5::{Digest, Md5};
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use regex::Regex;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use mindojo_core::config::Settings;
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::ollama::OllamaClient;
use mindojo_core::pipeline::MEMORIES_TABLE;
use mindojo_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

#[derive(Parser)]
#[command(name = "mindojo-import-triaged")]
#[command(about = "Import triaged memory candidates into vector store")]
struct Cli {
    /// Path to triaged report.json
    report: PathBuf,

    /// Show what would be imported without storing
    #[arg(long)]
    dry_run: bool,
}

/// Top-level triage report structure.
#[derive(Deserialize)]
struct TriageReport {
    findings: Option<Vec<FindingSection>>,
    triage: Option<TriageInfo>,
}

#[derive(Deserialize)]
struct FindingSection {
    #[serde(default)]
    findings: Vec<Finding>,
}

#[derive(Deserialize)]
struct Finding {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    recommendation: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    location: String,
}

#[derive(Deserialize)]
struct TriageInfo {
    #[serde(default)]
    triaged_by: String,
    #[serde(default)]
    triaged_at: String,
    #[serde(default)]
    decisions: Vec<TriageDecision>,
}

#[derive(Deserialize)]
struct TriageDecision {
    finding_id: String,
    action: String,
}

/// Parse `project:<name>` from recommendation text.
fn extract_project_from_recommendation(recommendation: &str) -> Option<String> {
    let re = Regex::new(r"project:(\S+)").ok()?;
    re.captures(recommendation)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Compute MD5 hex digest.
fn md5_hex(s: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[tokio::main]
async fn main() -> MindojoResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if !cli.report.exists() {
        return Err(MindojoError::Other(format!(
            "Report file not found: {}",
            cli.report.display()
        )));
    }

    let raw = std::fs::read_to_string(&cli.report)
        .with_context(|| format!("failed to read {}", cli.report.display()))?;
    let report: TriageReport = serde_json::from_str(&raw).context("failed to parse report JSON")?;

    let triage = report.triage.as_ref().ok_or_else(|| {
        MindojoError::Other("Report has no triage decisions. Run triage-findings first.".into())
    })?;

    println!("Processing triaged report: {}", cli.report.display());
    println!(
        "Triage by: {} at {}",
        if triage.triaged_by.is_empty() {
            "unknown"
        } else {
            &triage.triaged_by
        },
        if triage.triaged_at.is_empty() {
            "unknown"
        } else {
            &triage.triaged_at
        },
    );

    // Build finding_id -> finding map
    let mut findings_map = std::collections::HashMap::new();
    if let Some(sections) = &report.findings {
        for section in sections {
            for finding in &section.findings {
                findings_map.insert(finding.id.clone(), finding);
            }
        }
    }

    let settings = Settings::load();
    let ollama = OllamaClient::from_settings(&settings)?;

    let store = if !cli.dry_run {
        let s = LanceDbStore::open(&settings.lancedb_path).await?;
        s.ensure_table(MEMORIES_TABLE, settings.embed_dims).await?;
        Some(s)
    } else {
        None
    };

    let mut imported = 0u32;
    let mut skipped = 0u32;

    for decision in &triage.decisions {
        if decision.action != "fix" {
            skipped += 1;
            continue;
        }

        let Some(finding) = findings_map.get(&decision.finding_id) else {
            warn!(
                finding_id = %decision.finding_id,
                "Finding not found in report, skipping"
            );
            skipped += 1;
            continue;
        };

        // Determine scope from recommendation
        let project = extract_project_from_recommendation(&finding.recommendation);
        let user_id = project
            .as_ref()
            .map(|p| format!("project:{}", p))
            .unwrap_or_else(|| "global".to_string());

        // Build memory content
        let content = format!("{}\n\n{}", finding.title, finding.description);

        if cli.dry_run {
            println!("  [DRY RUN] Would import {} -> {}", finding.id, user_id);
            println!("    Title: {}", finding.title);
            println!("    Content length: {} chars", content.len());
        } else {
            let store = store.as_ref().unwrap();
            let vectors = ollama.embed(std::slice::from_ref(&content)).await?;

            let point_id = Uuid::new_v4().to_string();
            let now = Utc::now().to_rfc3339();

            let mut payload = serde_json::Map::new();
            payload.insert("data".into(), serde_json::Value::String(content.clone()));
            payload.insert("hash".into(), serde_json::Value::String(md5_hex(&content)));
            payload.insert("user_id".into(), serde_json::Value::String(user_id.clone()));
            payload.insert("created_at".into(), serde_json::Value::String(now));
            payload.insert("updated_at".into(), serde_json::Value::Null);
            payload.insert(
                "source_id".into(),
                serde_json::Value::String(finding.id.clone()),
            );
            payload.insert(
                "tags".into(),
                serde_json::Value::Array(
                    finding
                        .tags
                        .iter()
                        .map(|t| serde_json::Value::String(t.clone()))
                        .collect(),
                ),
            );
            payload.insert(
                "imported_from".into(),
                serde_json::Value::String(finding.location.clone()),
            );

            let point = VectorPoint {
                id: point_id,
                vector: vectors[0].clone(),
                payload: serde_json::Value::Object(payload),
            };

            store.upsert(MEMORIES_TABLE, &[point]).await?;
            println!(
                "  Imported {} -> {}: {}",
                finding.id, user_id, finding.title
            );
        }

        imported += 1;
    }

    println!("\nDone: {} imported, {} skipped", imported, skipped);
    Ok(())
}
