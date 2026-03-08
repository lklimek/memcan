//! Import triaged findings into vector store (moved from `mindojo-import-triaged`).

use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use mindojo_core::config::Settings;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::pipeline::{MEMORIES_TABLE, md5_hex};
use mindojo_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

use crate::ImportTriagedArgs;

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

fn extract_project_from_recommendation(recommendation: &str) -> Option<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"project:(\S+)").unwrap());
    re.captures(recommendation)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

pub async fn run(args: &ImportTriagedArgs) -> MindojoResult<()> {
    if !args.report.exists() {
        return Err(MindojoError::Other(format!(
            "Report file not found: {}",
            args.report.display()
        )));
    }

    let raw = std::fs::read_to_string(&args.report)
        .with_context(|| format!("failed to read {}", args.report.display()))?;
    let report: TriageReport = serde_json::from_str(&raw).context("failed to parse report JSON")?;

    let triage = report.triage.as_ref().ok_or_else(|| {
        MindojoError::Other("Report has no triage decisions. Run triage-findings first.".into())
    })?;

    println!("Processing triaged report: {}", args.report.display());
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

    let mut findings_map = std::collections::HashMap::new();
    if let Some(sections) = &report.findings {
        for section in sections {
            for finding in &section.findings {
                findings_map.insert(finding.id.clone(), finding);
            }
        }
    }

    let settings = Settings::load()?;
    settings.ensure_log_dir()?;
    let embedder = FastEmbedProvider::from_settings(&settings)?;

    let store = if !args.dry_run {
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
            warn!(finding_id = %decision.finding_id, "Finding not found in report, skipping");
            skipped += 1;
            continue;
        };

        let project = extract_project_from_recommendation(&finding.recommendation);
        let user_id = project
            .as_ref()
            .map(|p| format!("project:{}", p))
            .unwrap_or_else(|| "global".to_string());

        let content = format!("{}\n\n{}", finding.title, finding.description);

        if args.dry_run {
            println!("  [DRY RUN] Would import {} -> {}", finding.id, user_id);
            println!("    Title: {}", finding.title);
            println!("    Content length: {} chars", content.len());
        } else {
            let store = store.as_ref().unwrap();
            let vectors = embedder.embed(std::slice::from_ref(&content)).await?;
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
