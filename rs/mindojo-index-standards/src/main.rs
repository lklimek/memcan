//! mindojo-index-standards — Index markdown standards documents into vector store.
//!
//! Splits a markdown file on `##` and `###` headings, extracts metadata via LLM
//! (using the metadata-extraction prompt), embeds each chunk, and upserts into
//! the standards table.

use std::path::PathBuf;

use chrono::Utc;
use clap::Parser;
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use regex::Regex;
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use mindojo_core::config::Settings;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::llm::GenaiLlmProvider;
use mindojo_core::pipeline::STANDARDS_TABLE;
use mindojo_core::prompts::{METADATA_EXTRACTION_PROMPT, render_prompt};
use mindojo_core::traits::{
    EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, VectorPoint, VectorStore,
};

/// Valid standard types.
const VALID_TYPES: &[&str] = &["security", "coding", "cve", "guideline"];

/// Regex for safe section/ref IDs.
fn safe_id_re() -> Regex {
    Regex::new(r"^[A-Za-z0-9\-.:_/]+$").unwrap()
}

#[derive(Parser)]
#[command(name = "mindojo-index-standards")]
#[command(about = "Index markdown standards documents into vector store")]
struct Cli {
    /// Markdown file to index (required unless --drop)
    file: Option<PathBuf>,

    /// Standard identifier
    #[arg(long)]
    standard_id: String,

    /// Type of standard (security, coding, cve, guideline)
    #[arg(long)]
    standard_type: Option<String>,

    /// Standard version
    #[arg(long, default_value = "")]
    version: String,

    /// Language code
    #[arg(long, default_value = "en")]
    lang: String,

    /// Technology stack
    #[arg(long, default_value = "")]
    tech_stack: String,

    /// Source URL
    #[arg(long, default_value = "")]
    url: String,

    /// Ollama model for metadata extraction
    #[arg(long)]
    model: Option<String>,

    /// Drop all points for --standard-id
    #[arg(long)]
    drop: bool,

    /// LLM call timeout in seconds
    #[arg(long, default_value = "30")]
    llm_timeout: u64,

    /// Resume from chunk index
    #[arg(long, default_value = "0")]
    retry_from: usize,

    /// Enable debug logging
    #[arg(long)]
    verbose: bool,
}

/// A parsed chunk of markdown.
struct MdChunk {
    heading: String,
    parent_heading: String,
    level: usize,
    body: String,
}

/// LLM-extracted metadata for a chunk.
#[derive(Debug, Deserialize)]
struct ChunkMetadata {
    #[serde(default)]
    section_id: String,
    #[serde(default)]
    section_title: String,
    #[serde(default)]
    chapter: String,
    #[serde(default)]
    ref_ids: Vec<String>,
    #[serde(default)]
    code_patterns: String,
}

/// Split markdown on ## and ### headings, tracking hierarchy.
fn chunk_markdown(text: &str) -> Vec<MdChunk> {
    let heading_re = Regex::new(r"(?m)^(#{2,3})\s+(.+)").unwrap();
    let matches: Vec<_> = heading_re.find_iter(text).collect();

    if matches.is_empty() {
        return vec![MdChunk {
            heading: String::new(),
            parent_heading: String::new(),
            level: 0,
            body: text.trim().to_string(),
        }];
    }

    let mut chunks = Vec::new();

    // Preamble before first heading
    let preamble = text[..matches[0].start()].trim();
    if !preamble.is_empty() {
        chunks.push(MdChunk {
            heading: String::new(),
            parent_heading: String::new(),
            level: 0,
            body: preamble.to_string(),
        });
    }

    // Re-parse with captures to get level and heading text
    let captures: Vec<_> = heading_re.captures_iter(text).collect();
    let match_positions: Vec<_> = heading_re.find_iter(text).collect();

    let mut current_h2 = String::new();

    for (i, cap) in captures.iter().enumerate() {
        let level = cap[1].len();
        let heading = cap[2].trim().to_string();
        let start = match_positions[i].end();
        let end = if i + 1 < match_positions.len() {
            match_positions[i + 1].start()
        } else {
            text.len()
        };
        let body = text[start..end].trim().to_string();

        let parent = if level == 2 {
            current_h2 = heading.clone();
            String::new()
        } else {
            current_h2.clone()
        };

        chunks.push(MdChunk {
            heading,
            parent_heading: parent,
            level,
            body,
        });
    }

    chunks
}

/// Validate and sanitize LLM-produced metadata.
fn validate_metadata(mut meta: ChunkMetadata) -> ChunkMetadata {
    let safe_re = safe_id_re();

    // Clean ref_ids
    meta.ref_ids.retain(|id| safe_re.is_match(id));

    // Clean section_id
    if !meta.section_id.is_empty() && !safe_re.is_match(&meta.section_id) {
        meta.section_id = String::new();
    }

    // Truncate long strings
    if meta.section_title.len() > 200 {
        meta.section_title.truncate(200);
    }
    meta.section_title = meta.section_title.trim().to_string();

    if meta.chapter.len() > 200 {
        meta.chapter.truncate(200);
    }
    meta.chapter = meta.chapter.trim().to_string();

    meta
}

/// Extract metadata from a chunk using LLM.
async fn extract_metadata(
    chunk_text: &str,
    model: &str,
    llm: &dyn LlmProvider,
) -> MindojoResult<ChunkMetadata> {
    let prompt = render_prompt(METADATA_EXTRACTION_PROMPT, &[("chunk_text", chunk_text)]);

    let messages = vec![LlmMessage {
        role: "user".to_string(),
        content: prompt,
    }];

    let options = Some(LlmOptions {
        format_json: true,
        temperature: Some(0.0),
        max_tokens: Some(512),
        think: Some(false),
    });

    let response = llm.chat(model, &messages, options).await?;
    let meta: ChunkMetadata = serde_json::from_str(&response).with_context(|| {
        format!(
            "Failed to parse metadata: {}",
            &response[..response.len().min(200)]
        )
    })?;
    Ok(validate_metadata(meta))
}

/// Build fallback metadata from heading info.
fn fallback_metadata(heading: &str, parent_heading: &str) -> ChunkMetadata {
    ChunkMetadata {
        section_id: String::new(),
        section_title: heading.to_string(),
        chapter: parent_heading.to_string(),
        ref_ids: Vec::new(),
        code_patterns: String::new(),
    }
}

/// Reconstruct readable text from a chunk for embedding.
fn build_chunk_text(chunk: &MdChunk) -> String {
    let mut parts = Vec::new();
    if !chunk.heading.is_empty() {
        let prefix = if chunk.level == 2 { "##" } else { "###" };
        parts.push(format!("{} {}", prefix, chunk.heading));
    }
    if !chunk.body.is_empty() {
        parts.push(chunk.body.clone());
    }
    parts.join("\n\n")
}

#[tokio::main]
async fn main() -> MindojoResult<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .init();

    let settings = Settings::load();
    let model = cli.model.as_deref().unwrap_or(&settings.llm_model);

    let embedder = FastEmbedProvider::from_settings(&settings)?;
    let llm = GenaiLlmProvider::from_settings(&settings);
    let store = LanceDbStore::open(&settings.lancedb_path).await?;

    // Handle --drop mode
    if cli.drop {
        store
            .ensure_table(STANDARDS_TABLE, settings.embed_dims)
            .await?;
        let filter = format!(
            "JSON_EXTRACT(payload, '$.standard_id') = '{}'",
            cli.standard_id.replace('\'', "''")
        );

        let count = store.count(STANDARDS_TABLE, Some(&filter)).await?;
        if count == 0 {
            info!(standard_id = %cli.standard_id, "No points found");
            return Ok(());
        }

        let deleted = store.delete_by_filter(STANDARDS_TABLE, &filter).await?;
        info!(deleted, standard_id = %cli.standard_id, "Deleted points");
        return Ok(());
    }

    // Validate required args
    let file = cli
        .file
        .as_ref()
        .ok_or_else(|| MindojoError::Other("file is required unless --drop is specified".into()))?;
    let standard_type = cli.standard_type.as_deref().ok_or_else(|| {
        MindojoError::Other("--standard-type is required unless --drop is specified".into())
    })?;

    if !VALID_TYPES.contains(&standard_type) {
        return Err(MindojoError::Other(format!(
            "Invalid standard type '{}'. Must be one of: {}",
            standard_type,
            VALID_TYPES.join(", ")
        )));
    }

    if !file.is_file() {
        return Err(MindojoError::Other(format!(
            "File not found: {}",
            file.display()
        )));
    }

    let text = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let chunks = chunk_markdown(&text);
    info!(count = chunks.len(), file = %file.display(), "Parsed chunks");

    store
        .ensure_table(STANDARDS_TABLE, settings.embed_dims)
        .await?;

    let now = Utc::now().to_rfc3339();
    let mut errors: Vec<serde_json::Value> = Vec::new();
    let mut indexed = 0usize;

    for (chunk_index, chunk) in chunks.iter().enumerate() {
        if chunk_index < cli.retry_from {
            continue;
        }

        let chunk_text = build_chunk_text(chunk);
        if chunk_text.trim().is_empty() {
            debug!(chunk_index, "Skipping empty chunk");
            continue;
        }

        // Extract metadata with retry
        let meta = {
            let mut result = None;
            for attempt in 0..2 {
                match extract_metadata(&chunk_text, model, &llm).await {
                    Ok(m) => {
                        result = Some(m);
                        break;
                    }
                    Err(e) => {
                        if attempt == 0 {
                            warn!(chunk_index, error = %e, "LLM extraction failed (retrying)");
                        } else {
                            warn!(chunk_index, error = %e, "LLM extraction failed (using fallback)");
                        }
                    }
                }
            }
            result.unwrap_or_else(|| fallback_metadata(&chunk.heading, &chunk.parent_heading))
        };

        // Embed the chunk
        let vectors = match embedder.embed(std::slice::from_ref(&chunk_text)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(chunk_index, error = %e, "Embedding failed");
                errors.push(serde_json::json!({
                    "chunk_index": chunk_index,
                    "heading": chunk.heading,
                    "error": e.to_string(),
                }));
                continue;
            }
        };

        // Generate deterministic point ID
        let point_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}:{}", cli.standard_id, meta.section_id, chunk_index).as_bytes(),
        )
        .to_string();

        let mut payload = serde_json::Map::new();
        payload.insert("data".into(), serde_json::Value::String(chunk_text));
        payload.insert(
            "standard_id".into(),
            serde_json::Value::String(cli.standard_id.clone()),
        );
        payload.insert(
            "standard_type".into(),
            serde_json::Value::String(standard_type.to_string()),
        );
        payload.insert(
            "version".into(),
            serde_json::Value::String(cli.version.clone()),
        );
        payload.insert(
            "ref_ids".into(),
            serde_json::Value::Array(
                meta.ref_ids
                    .iter()
                    .map(|r| serde_json::Value::String(r.clone()))
                    .collect(),
            ),
        );
        payload.insert(
            "section_id".into(),
            serde_json::Value::String(meta.section_id),
        );
        payload.insert(
            "section_title".into(),
            serde_json::Value::String(meta.section_title.clone()),
        );
        payload.insert("chapter".into(), serde_json::Value::String(meta.chapter));
        payload.insert(
            "tech_stack".into(),
            serde_json::Value::String(cli.tech_stack.clone()),
        );
        payload.insert("lang".into(), serde_json::Value::String(cli.lang.clone()));
        payload.insert("url".into(), serde_json::Value::String(cli.url.clone()));
        payload.insert(
            "source_path".into(),
            serde_json::Value::String(file.display().to_string()),
        );
        payload.insert(
            "code_patterns".into(),
            serde_json::Value::String(meta.code_patterns),
        );
        payload.insert("indexed_at".into(), serde_json::Value::String(now.clone()));

        let point = VectorPoint {
            id: point_id,
            vector: vectors[0].clone(),
            payload: serde_json::Value::Object(payload),
        };

        if let Err(e) = store.upsert(STANDARDS_TABLE, &[point]).await {
            tracing::error!(chunk_index, error = %e, "Upsert failed");
            errors.push(serde_json::json!({
                "chunk_index": chunk_index,
                "heading": chunk.heading,
                "error": e.to_string(),
            }));
            continue;
        }

        indexed += 1;
        info!(
            chunk_index,
            total = chunks.len() - 1,
            title = if meta.section_title.is_empty() {
                "(untitled)"
            } else {
                &meta.section_title
            },
            "Indexed chunk"
        );
    }

    info!(indexed, errors = errors.len(), "Indexing complete");

    if !errors.is_empty() {
        let error_json = serde_json::to_string_pretty(&errors)?;
        let error_path = PathBuf::from("index-standards-errors.json");
        std::fs::write(&error_path, error_json)?;
        warn!(path = %error_path.display(), "Errors written");
    }

    if errors.is_empty() {
        Ok(())
    } else {
        return Err(MindojoError::Other(format!(
            "{} chunks failed",
            errors.len()
        )));
    }
}
