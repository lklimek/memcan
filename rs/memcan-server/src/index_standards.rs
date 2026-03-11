//! CLI wrapper for standards indexing. Delegates to `memcan_core::indexing::standards`.

use tracing::info;

use memcan_core::error::{MemcanError, Result as MemcanResult};
use memcan_core::indexing::standards::{
    IndexStandardsParams, VALID_TYPES, drop_standards, index_standards,
};
use memcan_core::init::{MemcanContext, create_llm_provider};

use crate::IndexStandardsArgs;

pub async fn run(args: &IndexStandardsArgs) -> MemcanResult<()> {
    let ctx = MemcanContext::init().await?;

    if args.drop {
        drop_standards(&args.standard_id, &ctx.store, ctx.settings.embed_dims).await?;
        return Ok(());
    }

    ctx.init_llm().await?;
    let (llm, default_model) = create_llm_provider(&ctx.settings);
    let model = args.model.as_deref().unwrap_or(&default_model);

    let file = args
        .file
        .as_ref()
        .ok_or_else(|| MemcanError::Other("file is required unless --drop is specified".into()))?;
    let standard_type = args.standard_type.as_deref().ok_or_else(|| {
        MemcanError::Other("--standard-type is required unless --drop is specified".into())
    })?;

    if !VALID_TYPES.contains(&standard_type) {
        return Err(MemcanError::Other(format!(
            "Invalid standard type '{}'. Must be one of: {}",
            standard_type,
            VALID_TYPES.join(", ")
        )));
    }

    if !file.is_file() {
        return Err(MemcanError::Other(format!(
            "File not found: {}",
            file.display()
        )));
    }

    let content = std::fs::read_to_string(file)
        .map_err(|e| MemcanError::Other(format!("failed to read {}: {e}", file.display())))?;

    let params = IndexStandardsParams {
        content,
        standard_id: args.standard_id.clone(),
        standard_type: standard_type.to_string(),
        version: args.version.as_deref().unwrap_or("").to_string(),
        lang: args.lang.as_deref().unwrap_or("en").to_string(),
        url: args.url.as_deref().unwrap_or("").to_string(),
    };

    let result = index_standards(
        &params,
        &ctx.store,
        &ctx.embedder,
        llm.as_ref(),
        model,
        ctx.settings.embed_dims,
    )
    .await?;

    info!(
        indexed = result.indexed,
        errors = result.errors.len(),
        "Indexing complete"
    );

    if !result.errors.is_empty() {
        let error_json: Vec<serde_json::Value> = result
            .errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "chunk_index": e.chunk_index,
                    "heading": e.heading,
                    "error": e.error,
                })
            })
            .collect();
        let json_str = serde_json::to_string_pretty(&error_json)?;
        let error_path = std::env::temp_dir().join("index-standards-errors.json");
        std::fs::write(&error_path, json_str)?;
        tracing::warn!(path = %error_path.display(), "Errors written");
        return Err(MemcanError::Other(format!(
            "{} chunks failed",
            result.errors.len()
        )));
    }

    Ok(())
}
