//! Index markdown standards documents into vector storage.
//!
//! Chunks a markdown document by headings, extracts metadata via LLM,
//! embeds each chunk, and upserts to the vector store.

use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{MemcanError, Result, ResultExt};
use crate::pipeline::{STANDARDS_TABLE, chunk_content, resolve_context_budget};
use crate::prompts::{METADATA_EXTRACTION_PROMPT, render_prompt};
use crate::traits::{
    EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, Role, VectorPoint, VectorStore,
};

/// Accepted standard types for validation.
pub const VALID_TYPES: &[&str] = &["security", "coding", "cve", "guideline"];

/// Compiled regex for safe identifier validation (cached via `OnceLock`).
fn safe_id_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[A-Za-z0-9\-.:_/]+$").unwrap())
}

/// Metadata extracted from a standards chunk by the LLM.
#[derive(Debug, Deserialize)]
pub struct ChunkMetadata {
    #[serde(default)]
    pub section_id: String,
    #[serde(default)]
    pub section_title: String,
    #[serde(default)]
    pub chapter: String,
    #[serde(default)]
    pub ref_ids: Vec<String>,
    #[serde(default)]
    pub code_patterns: String,
}

/// A single chunk produced by splitting a markdown document on headings.
pub struct MdChunk {
    pub heading: String,
    pub parent_heading: String,
    pub level: usize,
    pub body: String,
}

/// Split a markdown document into chunks on `##` / `###` headings.
///
/// Any text before the first heading becomes a preamble chunk with an empty
/// heading. Parent-child relationships are tracked so `###` sections know
/// which `##` they belong to.
pub fn chunk_markdown(text: &str) -> Vec<MdChunk> {
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
    let preamble = text[..matches[0].start()].trim();
    if !preamble.is_empty() {
        chunks.push(MdChunk {
            heading: String::new(),
            parent_heading: String::new(),
            level: 0,
            body: preamble.to_string(),
        });
    }

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

/// Sanitize LLM-extracted metadata: drop unsafe IDs, truncate long fields.
pub fn validate_metadata(mut meta: ChunkMetadata) -> ChunkMetadata {
    let safe_re = safe_id_re();
    meta.ref_ids.retain(|id| safe_re.is_match(id));
    if !meta.section_id.is_empty() && !safe_re.is_match(&meta.section_id) {
        meta.section_id = String::new();
    }
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

/// Extract structured metadata from a chunk using the LLM.
///
/// When the rendered prompt exceeds `budget_tokens`, the chunk text is
/// sub-chunked and only the first sub-chunk (which typically contains the
/// heading and opening context) is sent for metadata extraction.
/// Retries once on failure, then falls back to [`fallback_metadata`].
pub async fn extract_metadata(
    chunk_text: &str,
    model: &str,
    llm: &dyn LlmProvider,
    budget_tokens: usize,
) -> Result<ChunkMetadata> {
    let text_for_extraction = {
        let probe_prompt = render_prompt(METADATA_EXTRACTION_PROMPT, &[("chunk_text", "")]);
        let sub_chunks = chunk_content(chunk_text, &probe_prompt, budget_tokens);
        if sub_chunks.len() > 1 {
            debug!(
                original_len = chunk_text.len(),
                sub_chunks = sub_chunks.len(),
                "chunk exceeds budget, using first sub-chunk for metadata"
            );
        }
        sub_chunks.first().copied().unwrap_or(chunk_text)
    };

    let prompt = render_prompt(
        METADATA_EXTRACTION_PROMPT,
        &[("chunk_text", text_for_extraction)],
    );
    let messages = vec![LlmMessage {
        role: Role::User,
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

/// Produce fallback metadata from heading text when LLM extraction fails.
pub fn fallback_metadata(heading: &str, parent_heading: &str) -> ChunkMetadata {
    ChunkMetadata {
        section_id: String::new(),
        section_title: heading.to_string(),
        chapter: parent_heading.to_string(),
        ref_ids: Vec::new(),
        code_patterns: String::new(),
    }
}

/// Reconstruct the text of a chunk with its markdown heading prefix.
pub fn build_chunk_text(chunk: &MdChunk) -> String {
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

/// Parameters for indexing a standards markdown document.
pub struct IndexStandardsParams {
    pub content: String,
    pub standard_id: String,
    pub standard_type: String,
    pub version: String,
    pub lang: String,
    pub url: String,
}

/// Result of an indexing operation.
pub struct IndexStandardsResult {
    pub indexed: usize,
    pub errors: Vec<IndexChunkError>,
}

/// Error for a single chunk that failed during indexing.
pub struct IndexChunkError {
    pub chunk_index: usize,
    pub heading: String,
    pub error: String,
}

/// Index a standards markdown document.
///
/// Chunks by headings, extracts metadata via LLM, embeds each chunk, and
/// upserts to the vector store. Returns the count of indexed chunks and any
/// per-chunk errors.
pub async fn index_standards(
    params: &IndexStandardsParams,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    llm: &dyn LlmProvider,
    llm_model: &str,
    embed_dims: usize,
) -> Result<IndexStandardsResult> {
    if !VALID_TYPES.contains(&params.standard_type.as_str()) {
        return Err(MemcanError::Other(format!(
            "Invalid standard type '{}'. Must be one of: {}",
            params.standard_type,
            VALID_TYPES.join(", ")
        )));
    }

    let chunks = chunk_markdown(&params.content);
    info!(count = chunks.len(), "Parsed markdown chunks");

    store.ensure_table(STANDARDS_TABLE, embed_dims).await?;

    let budget = resolve_context_budget(llm, llm_model).await;
    let now = Utc::now().to_rfc3339();
    let mut errors = Vec::new();
    let mut indexed = 0usize;

    for (chunk_index, chunk) in chunks.iter().enumerate() {
        let chunk_text = build_chunk_text(chunk);
        if chunk_text.trim().is_empty() {
            debug!(chunk_index, "Skipping empty chunk");
            continue;
        }

        let meta = {
            let mut result = None;
            for attempt in 0..2 {
                match extract_metadata(&chunk_text, llm_model, llm, budget).await {
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

        let vectors = match embedder.embed(std::slice::from_ref(&chunk_text)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(chunk_index, error = %e, "Embedding failed");
                errors.push(IndexChunkError {
                    chunk_index,
                    heading: chunk.heading.clone(),
                    error: e.to_string(),
                });
                continue;
            }
        };

        let point_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("{}:{}:{}", params.standard_id, meta.section_id, chunk_index).as_bytes(),
        )
        .to_string();

        let mut payload = serde_json::Map::new();
        payload.insert("data".into(), serde_json::Value::String(chunk_text));
        payload.insert(
            "standard_id".into(),
            serde_json::Value::String(params.standard_id.clone()),
        );
        payload.insert(
            "standard_type".into(),
            serde_json::Value::String(params.standard_type.clone()),
        );
        payload.insert(
            "version".into(),
            serde_json::Value::String(params.version.clone()),
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
            serde_json::Value::String(String::new()),
        );
        payload.insert(
            "lang".into(),
            serde_json::Value::String(params.lang.clone()),
        );
        payload.insert("url".into(), serde_json::Value::String(params.url.clone()));
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
            errors.push(IndexChunkError {
                chunk_index,
                heading: chunk.heading.clone(),
                error: e.to_string(),
            });
            continue;
        }

        indexed += 1;
        info!(
            chunk_index,
            total = chunks.len(),
            title = if meta.section_title.is_empty() {
                "(untitled)"
            } else {
                &meta.section_title
            },
            "Indexed chunk"
        );
    }

    Ok(IndexStandardsResult { indexed, errors })
}

/// Drop all indexed standards for a given `standard_id`.
///
/// Returns the number of deleted records.
pub async fn drop_standards(
    standard_id: &str,
    store: &dyn VectorStore,
    embed_dims: usize,
) -> Result<usize> {
    store.ensure_table(STANDARDS_TABLE, embed_dims).await?;
    let filter = format!("standard_id = '{}'", standard_id.replace('\'', "''"));
    let count = store.count(STANDARDS_TABLE, Some(&filter)).await?;
    if count == 0 {
        info!(standard_id, "No points found");
        return Ok(0);
    }
    let deleted = store.delete_by_filter(STANDARDS_TABLE, &filter).await?;
    info!(deleted, standard_id, "Deleted points");
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_markdown_no_headings() {
        let text = "Just some plain text.";
        let chunks = chunk_markdown(text);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].heading.is_empty());
        assert_eq!(chunks[0].body, "Just some plain text.");
    }

    #[test]
    fn test_chunk_markdown_with_headings() {
        let text = "## First\nBody one\n## Second\nBody two";
        let chunks = chunk_markdown(text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "First");
        assert_eq!(chunks[1].heading, "Second");
    }

    #[test]
    fn test_chunk_markdown_preamble() {
        let text = "Preamble text\n\n## Heading\nBody";
        let chunks = chunk_markdown(text);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].heading.is_empty());
        assert_eq!(chunks[0].body, "Preamble text");
        assert_eq!(chunks[1].heading, "Heading");
    }

    #[test]
    fn test_chunk_markdown_nested_headings() {
        let text = "## Parent\nParent body\n### Child\nChild body";
        let chunks = chunk_markdown(text);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading, "Parent");
        assert!(chunks[0].parent_heading.is_empty());
        assert_eq!(chunks[1].heading, "Child");
        assert_eq!(chunks[1].parent_heading, "Parent");
    }

    #[test]
    fn test_validate_metadata_strips_unsafe_ids() {
        let meta = ChunkMetadata {
            section_id: "valid-id.1".into(),
            section_title: "Title".into(),
            chapter: "Ch".into(),
            ref_ids: vec!["good-1".into(), "bad id with spaces".into()],
            code_patterns: String::new(),
        };
        let validated = validate_metadata(meta);
        assert_eq!(validated.section_id, "valid-id.1");
        assert_eq!(validated.ref_ids.len(), 1);
        assert_eq!(validated.ref_ids[0], "good-1");
    }

    #[test]
    fn test_validate_metadata_clears_unsafe_section_id() {
        let meta = ChunkMetadata {
            section_id: "has spaces".into(),
            section_title: "T".into(),
            chapter: String::new(),
            ref_ids: vec![],
            code_patterns: String::new(),
        };
        let validated = validate_metadata(meta);
        assert!(validated.section_id.is_empty());
    }

    #[test]
    fn test_validate_metadata_truncates_long_title() {
        let meta = ChunkMetadata {
            section_id: String::new(),
            section_title: "x".repeat(300),
            chapter: "y".repeat(300),
            ref_ids: vec![],
            code_patterns: String::new(),
        };
        let validated = validate_metadata(meta);
        assert_eq!(validated.section_title.len(), 200);
        assert_eq!(validated.chapter.len(), 200);
    }

    #[test]
    fn test_fallback_metadata() {
        let meta = fallback_metadata("Heading", "Parent");
        assert_eq!(meta.section_title, "Heading");
        assert_eq!(meta.chapter, "Parent");
        assert!(meta.section_id.is_empty());
        assert!(meta.ref_ids.is_empty());
    }

    #[test]
    fn test_build_chunk_text_h2() {
        let chunk = MdChunk {
            heading: "Title".into(),
            parent_heading: String::new(),
            level: 2,
            body: "Body text".into(),
        };
        assert_eq!(build_chunk_text(&chunk), "## Title\n\nBody text");
    }

    #[test]
    fn test_build_chunk_text_h3() {
        let chunk = MdChunk {
            heading: "Sub".into(),
            parent_heading: "Parent".into(),
            level: 3,
            body: "Content".into(),
        };
        assert_eq!(build_chunk_text(&chunk), "### Sub\n\nContent");
    }

    #[test]
    fn test_build_chunk_text_no_heading() {
        let chunk = MdChunk {
            heading: String::new(),
            parent_heading: String::new(),
            level: 0,
            body: "Just body".into(),
        };
        assert_eq!(build_chunk_text(&chunk), "Just body");
    }

    #[test]
    fn test_valid_types() {
        assert!(VALID_TYPES.contains(&"security"));
        assert!(VALID_TYPES.contains(&"coding"));
        assert!(VALID_TYPES.contains(&"cve"));
        assert!(VALID_TYPES.contains(&"guideline"));
        assert!(!VALID_TYPES.contains(&"random"));
    }
}
