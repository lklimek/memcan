//! Import JSON exports into LanceDB vector store (moved from `memcan-migrate`).

use serde::Deserialize;
use tracing::info;

use memcan_core::error::{MemcanError, Result as MemcanResult, ResultExt};
use memcan_core::init::MemcanContext;
use memcan_core::pipeline::MEMORIES_TABLE;
use memcan_core::schema::MemcanTableSchema;
use memcan_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

use crate::MigrateArgs;

#[derive(Deserialize)]
struct ExportRecord {
    id: String,
    #[serde(default)]
    data: String,
    #[serde(default)]
    user_id: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

const BATCH_SIZE: usize = 50;

pub async fn run(args: &MigrateArgs) -> MemcanResult<()> {
    if !args.export_file.exists() {
        return Err(MemcanError::Other(format!(
            "Export file not found: {}",
            args.export_file.display()
        )));
    }

    let raw = std::fs::read_to_string(&args.export_file)
        .with_context(|| format!("failed to read {}", args.export_file.display()))?;
    let records: Vec<ExportRecord> =
        serde_json::from_str(&raw).context("failed to parse export JSON (expected array)")?;

    info!(count = records.len(), "Found records in export");

    if records.is_empty() {
        info!("Nothing to migrate");
        return Ok(());
    }

    if args.dry_run {
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
            info!(user_id = uid, data = data_preview, "Would migrate record");
        }
        info!(
            count = records.len(),
            "Dry run complete. Re-run without --dry-run to migrate."
        );
        return Ok(());
    }

    let ctx = MemcanContext::init().await?;
    let ts = MemcanTableSchema;
    ctx.store
        .ensure_table(MEMORIES_TABLE, ctx.settings.embed_dims, &ts)
        .await?;

    let texts: Vec<String> = records.iter().map(|r| r.data.clone()).collect();
    info!(count = texts.len(), model = %ctx.settings.embed_model, "Embedding texts");

    let mut all_embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for batch_start in (0..texts.len()).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(texts.len());
        let batch_texts = &texts[batch_start..batch_end];
        let batch_embeddings = ctx.embedder.embed(batch_texts).await?;
        all_embeddings.extend(batch_embeddings);
        info!(
            progress = all_embeddings.len(),
            total = texts.len(),
            "Embedded batch"
        );
    }

    if all_embeddings.len() != records.len() {
        return Err(MemcanError::Other(format!(
            "Embedding count mismatch: got {} embeddings for {} records",
            all_embeddings.len(),
            records.len()
        )));
    }
    if all_embeddings[0].len() != ctx.settings.embed_dims {
        return Err(MemcanError::DimensionMismatch {
            expected: ctx.settings.embed_dims,
            actual: all_embeddings[0].len(),
        });
    }

    let mut total_upserted = 0usize;
    for batch_start in (0..records.len()).step_by(BATCH_SIZE) {
        let batch_end = (batch_start + BATCH_SIZE).min(records.len());
        let points: Vec<VectorPoint> = records[batch_start..batch_end]
            .iter()
            .zip(&all_embeddings[batch_start..batch_end])
            .map(|(record, embedding)| {
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

        ctx.store.upsert(MEMORIES_TABLE, &points, &ts).await?;
        total_upserted += points.len();
        info!(
            progress = total_upserted,
            total = records.len(),
            "Upserted batch"
        );
    }

    let final_count = ctx.store.count(MEMORIES_TABLE, None).await?;
    info!(
        records = final_count,
        table = MEMORIES_TABLE,
        "Migration complete"
    );

    Ok(())
}
