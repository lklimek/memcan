//! Purge memories matching a source (and optionally project) filter.
//!
//! `source` is stored inside the JSON `payload` column, not as a top-level
//! LanceDB column, so we scroll records in batches and filter in-process.

use memcan_core::error::Result as MemcanResult;
use memcan_core::init::MemcanContext;
use memcan_core::pipeline::MEMORIES_TABLE;
use memcan_core::traits::VectorStore;

use crate::PurgeMemoriesArgs;

const BATCH_SIZE: usize = 1000;

pub async fn run(args: &PurgeMemoriesArgs) -> MemcanResult<()> {
    let ctx = MemcanContext::init().await?;

    // Build an optional top-level user_id filter (this column exists in the schema).
    let user_id_filter = args
        .project
        .as_ref()
        .map(|p| format!("user_id = 'project:{}'", p.replace('\'', "''")));

    // Scroll through all (optionally pre-filtered) records and match on source in payload.
    let mut matching_ids: Vec<String> = Vec::new();
    let mut offset = 0usize;
    let source = args.source.as_str();

    loop {
        let batch = ctx
            .store
            .scroll(
                MEMORIES_TABLE,
                user_id_filter.as_deref(),
                BATCH_SIZE,
                offset,
            )
            .await?;
        let n = batch.len();
        for record in batch {
            let payload_source = record
                .payload
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if payload_source == source {
                matching_ids.push(record.id);
            }
        }
        if n < BATCH_SIZE {
            break;
        }
        offset += BATCH_SIZE;
    }

    if args.dry_run {
        println!(
            "Dry run: {} record(s) would be deleted (source={}{})",
            matching_ids.len(),
            source,
            args.project
                .as_ref()
                .map(|p| format!(", project={p}"))
                .unwrap_or_default(),
        );
        return Ok(());
    }

    let total = matching_ids.len();
    if total == 0 {
        println!("No records matched source={source}");
        return Ok(());
    }

    // Delete in batches to avoid overly long SQL IN clauses.
    let mut deleted = 0usize;
    for chunk in matching_ids.chunks(500) {
        ctx.store.delete(MEMORIES_TABLE, chunk).await?;
        deleted += chunk.len();
    }

    println!(
        "Deleted {deleted} record(s) (source={}{})",
        source,
        args.project
            .as_ref()
            .map(|p| format!(", project={p}"))
            .unwrap_or_default(),
    );

    Ok(())
}
