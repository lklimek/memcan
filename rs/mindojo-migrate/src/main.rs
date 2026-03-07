//! mindojo-migrate — Import JSON exports into LanceDB vector store.
//!
//! Reads a JSON export (array of records with id, data, user_id, etc.),
//! re-embeds with the current embedding model, and upserts into LanceDB.

use std::path::PathBuf;

use clap::Parser;
use serde::Deserialize;

use mindojo_core::config::Settings;
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::ollama::OllamaClient;
use mindojo_core::pipeline::MEMORIES_TABLE;
use mindojo_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

#[derive(Parser)]
#[command(name = "mindojo-migrate")]
#[command(about = "Import JSON export into LanceDB vector store")]
struct Cli {
    /// Path to JSON export file
    export_file: PathBuf,

    /// Print plan without writing
    #[arg(long)]
    dry_run: bool,
}

/// A single record from the JSON export.
#[derive(Deserialize)]
struct ExportRecord {
    id: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    user_id: String,
    /// Flatten remaining fields into the payload.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Batch size for embedding and upserting.
const BATCH_SIZE: usize = 50;

#[tokio::main]
async fn main() -> MindojoResult<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    if !cli.export_file.exists() {
        return Err(MindojoError::Other(format!(
            "Export file not found: {}",
            cli.export_file.display()
        )));
    }

    let raw = std::fs::read_to_string(&cli.export_file)
        .with_context(|| format!("failed to read {}", cli.export_file.display()))?;

    let records: Vec<ExportRecord> = serde_json::from_str(&raw)
        .context("failed to parse export JSON (expected array)")?;

    println!("Found {} records in export.", records.len());

    if records.is_empty() {
        println!("Nothing to migrate.");
        return Ok(());
    }

    if cli.dry_run {
        for record in &records {
            let data_preview = if record.data.len() > 80 {
                &record.data[..80]
            } else {
                &record.data
            };
            let uid = if record.user_id.is_empty() {
                "?"
            } else {
                &record.user_id
            };
            println!("  [{}] {}", uid, data_preview);
        }
        println!(
            "\nDry run: would migrate {} records. Re-run without --dry-run.",
            records.len()
        );
        return Ok(());
    }

    let settings = Settings::load();
    let ollama = OllamaClient::from_settings(&settings)?;
    let store = LanceDbStore::open(&settings.lancedb_path).await?;

    store
        .ensure_table(MEMORIES_TABLE, settings.embed_dims)
        .await?;

    // Embed all texts
    let texts: Vec<String> = records.iter().map(|r| r.data.clone()).collect();

    println!(
        "Embedding {} texts with {}...",
        texts.len(),
        settings.embed_model
    );

    // Process in batches to avoid memory issues
    let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

    for batch_start in (0..texts.len()).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(texts.len());
        let batch_texts = &texts[batch_start..batch_end];

        let batch_embeddings = ollama.embed(batch_texts).await.map_err(|e| {
            MindojoError::Other(format!("embedding batch starting at {batch_start}: {e}"))
        })?;

        all_embeddings.extend(batch_embeddings);
        println!("  Embedded {}/{}", all_embeddings.len(), texts.len());
    }

    if all_embeddings.len() != records.len() {
        return Err(MindojoError::Other(format!(
            "Embedding count mismatch: got {} embeddings for {} records",
            all_embeddings.len(),
            records.len()
        )));
    }
    if all_embeddings[0].len() != settings.embed_dims {
        return Err(MindojoError::DimensionMismatch {
            expected: settings.embed_dims,
            actual: all_embeddings[0].len(),
        });
    }

    // Upsert in batches
    let mut total_upserted = 0usize;
    for batch_start in (0..records.len()).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(records.len());

        let points: Vec<VectorPoint> = records[batch_start..batch_end]
            .iter()
            .zip(&all_embeddings[batch_start..batch_end])
            .map(|(record, embedding)| {
                // Build payload from all record fields
                let mut payload = record.extra.clone();
                payload.insert(
                    "data".into(),
                    serde_json::Value::String(record.data.clone()),
                );
                payload.insert(
                    "user_id".into(),
                    serde_json::Value::String(record.user_id.clone()),
                );

                VectorPoint {
                    id: record.id.clone(),
                    vector: embedding.clone(),
                    payload: serde_json::Value::Object(payload),
                }
            })
            .collect();

        store.upsert(MEMORIES_TABLE, &points).await?;
        total_upserted += points.len();
        println!("  Upserted {}/{}", total_upserted, records.len());
    }

    // Verify
    let final_count = store.count(MEMORIES_TABLE, None).await?;
    println!(
        "\nMigration complete: {} records in '{}'.",
        final_count, MEMORIES_TABLE
    );

    Ok(())
}
