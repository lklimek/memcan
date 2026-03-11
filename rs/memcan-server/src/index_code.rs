//! CLI wrapper for code indexing. Delegates to `memcan_core::indexing::code`.

use tracing::info;

use memcan_core::error::{MemcanError, Result as MemcanResult};
use memcan_core::indexing::code::{IndexCodeParams, drop_code, index_code};
use memcan_core::init::{MemcanContext, create_llm_provider};

use crate::IndexCodeArgs;

pub async fn run(args: &IndexCodeArgs) -> MemcanResult<()> {
    let ctx = MemcanContext::init().await?;

    if args.drop {
        drop_code(&args.project, &ctx.store, ctx.settings.embed_dims).await?;
        return Ok(());
    }

    let tech_stack = args.tech_stack.as_deref().ok_or_else(|| {
        MemcanError::Other("--tech-stack is required unless --drop is specified".into())
    })?;

    let (llm, llm_model) = create_llm_provider(&ctx.settings);

    let params = IndexCodeParams {
        project_dir: args.project_dir.clone(),
        project: args.project.clone(),
        tech_stack: tech_stack.to_string(),
        max_file_size: args.max_file_size,
    };

    let result = index_code(
        &params,
        &ctx.store,
        &ctx.embedder,
        llm.as_ref(),
        &llm_model,
        ctx.settings.embed_dims,
    )
    .await?;

    info!(
        upserted = result.upserted,
        unchanged = result.unchanged,
        errors = result.errors,
        deleted = result.deleted,
        "Indexing complete"
    );

    Ok(())
}
