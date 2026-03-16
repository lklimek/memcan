//! CLI wrapper for collection export to JSONL.

use std::io::Write;

use tracing::info;

use memcan_core::error::Result as MemcanResult;
use memcan_core::export::{self, record_to_jsonl};
use memcan_core::init::MemcanContext;

use crate::ExportArgs;

pub async fn run(args: &ExportArgs) -> MemcanResult<()> {
    let ctx = MemcanContext::init().await?;

    let table = export::collection_to_table(&args.collection)?;

    let mut writer: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(std::fs::File::create(path).map_err(|e| {
            memcan_core::error::MemcanError::Other(format!("cannot create {}: {e}", path.display()))
        })?),
        None => Box::new(std::io::stdout().lock()),
    };

    let mut count = 0usize;
    export::export_collection(
        &ctx.store,
        table,
        args.filter.as_deref(),
        1000,
        &mut |record| {
            let line = record_to_jsonl(&record)?;
            writeln!(writer, "{line}").map_err(|e| {
                memcan_core::error::MemcanError::Other(format!("write failed: {e}"))
            })?;
            count += 1;
            Ok(())
        },
    )
    .await?;

    eprintln!("Exported {count} records from '{}'", args.collection);
    info!(count, collection = %args.collection, "Export complete");

    Ok(())
}
