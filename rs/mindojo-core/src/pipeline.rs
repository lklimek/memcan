//! Memory storage pipeline: extract facts via LLM, deduplicate, store in vector DB.
//!
//! This module ports the Python `memory_pipeline.py` logic. All backend
//! interactions go through trait objects, making the pipeline storage- and
//! LLM-agnostic.

use std::collections::HashSet;

use chrono::Utc;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{Result, ResultExt};
use crate::prompts::{FACT_EXTRACTION_PROMPT, MEMORY_UPDATE_PROMPT, render_prompt};
use crate::traits::{
    EmbeddingProvider, LlmMessage, LlmOptions, LlmProvider, Role, VectorPoint, VectorStore,
};

/// Table name for user memories.
pub const MEMORIES_TABLE: &str = "mindojo_memories";

/// Table name for standards documents.
pub const STANDARDS_TABLE: &str = "mindojo_standards";

/// Table name for indexed code.
pub const CODE_TABLE: &str = "mindojo_code";

/// Reserved payload keys that user metadata must not overwrite.
const RESERVED_KEYS: &[&str] = &["data", "hash", "user_id", "created_at", "updated_at"];

/// Max length of a single extracted fact (chars). Longer facts are truncated.
const MAX_FACT_LENGTH: usize = 2000;

/// Max number of facts returned from a single extraction.
const MAX_FACTS_PER_EXTRACTION: usize = 50;

// INTENTIONAL(SEC-009): MD5 used for content deduplication only, not security.
// Collision risk negligible for this use case.
/// Compute MD5 hex digest of a string.
pub fn md5_hex(data: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Strip reserved keys from user-supplied metadata.
fn clean_metadata(metadata: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let reserved: HashSet<&str> = RESERVED_KEYS.iter().copied().collect();
    match metadata.as_object() {
        Some(map) => map
            .iter()
            .filter(|(k, _)| !reserved.contains(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        None => serde_json::Map::new(),
    }
}

/// LLM response for fact extraction.
#[derive(Debug, Deserialize)]
struct FactsResponse {
    #[serde(default)]
    facts: Vec<String>,
}

/// LLM response for memory update/dedup.
#[derive(Debug, Deserialize)]
struct MemoryUpdateResponse {
    #[serde(default)]
    events: Vec<MemoryEvent>,
}

/// Type of memory update operation returned by the dedup LLM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum EventType {
    Add,
    Update,
    Delete,
    None,
}

/// A single memory update event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    #[serde(rename = "type")]
    pub event_type: EventType,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub memory_id: Option<String>,
}

/// Validate and sanitize extracted facts: truncate long facts and cap total count.
fn validate_facts(facts: Vec<String>) -> Vec<String> {
    let capped = if facts.len() > MAX_FACTS_PER_EXTRACTION {
        warn!(
            count = facts.len(),
            max = MAX_FACTS_PER_EXTRACTION,
            "truncating fact count to limit"
        );
        &facts[..MAX_FACTS_PER_EXTRACTION]
    } else {
        &facts[..]
    };

    capped
        .iter()
        .map(|f| {
            if f.len() > MAX_FACT_LENGTH {
                warn!(
                    length = f.len(),
                    max = MAX_FACT_LENGTH,
                    "truncating oversized fact"
                );
                let mut truncated = f[..MAX_FACT_LENGTH].to_string();
                truncated.push_str("...");
                truncated
            } else {
                f.clone()
            }
        })
        .collect()
}

/// Extract individual facts from content using the LLM.
pub async fn extract_facts(
    content: &str,
    llm: &dyn LlmProvider,
    llm_model: &str,
    extraction_prompt: Option<&str>,
) -> Result<Option<Vec<String>>> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let prompt = extraction_prompt.unwrap_or(FACT_EXTRACTION_PROMPT);
    let rendered = render_prompt(prompt, &[("today", &today)]);

    let messages = vec![
        LlmMessage {
            role: Role::System,
            content: rendered,
        },
        LlmMessage {
            role: Role::User,
            content: content.to_string(),
        },
    ];

    let options = Some(LlmOptions {
        format_json: true,
        ..Default::default()
    });

    match llm.chat(llm_model, &messages, options).await {
        Ok(response) => match serde_json::from_str::<FactsResponse>(&response) {
            Ok(parsed) => Ok(Some(validate_facts(parsed.facts))),
            Err(e) => {
                warn!("fact extraction JSON parse failed: {e}");
                Ok(None)
            }
        },
        Err(e) => {
            warn!("fact extraction LLM call failed: {e}");
            Ok(None)
        }
    }
}

/// Deduplicate facts against existing memories and store.
#[allow(clippy::too_many_arguments)]
pub async fn dedup_and_store(
    facts: &[String],
    user_id: &str,
    metadata: &serde_json::Value,
    table_name: &str,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    llm: &dyn LlmProvider,
    llm_model: &str,
) -> Result<()> {
    let meta = clean_metadata(metadata);

    for fact in facts {
        let vectors = embedder.embed(std::slice::from_ref(fact)).await?;
        let vector = &vectors[0];

        let filter = format!("user_id = '{}'", user_id.replace('\'', "''"));
        let existing = store.search(table_name, vector, Some(&filter), 5).await?;

        let existing_memories: Vec<serde_json::Value> = existing
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "text": r.payload.get("data").and_then(|v| v.as_str()).unwrap_or(""),
                })
            })
            .collect();

        let events = match run_dedup_llm(&existing_memories, fact, llm, llm_model).await {
            Ok(events) => events,
            Err(e) => {
                warn!("dedup LLM failed, falling back to ADD: {e}");
                vec![MemoryEvent {
                    event_type: EventType::Add,
                    data: Some(fact.clone()),
                    memory_id: None,
                }]
            }
        };

        for event in events {
            match event.event_type {
                EventType::Add => {
                    let data = event.data.as_deref().unwrap_or(fact);
                    let now = Utc::now().to_rfc3339();
                    let hash = md5_hex(data);
                    let mut payload = json!({
                        "data": data,
                        "hash": hash,
                        "content_hash": hash,
                        "user_id": user_id,
                        "created_at": now,
                        "updated_at": serde_json::Value::Null,
                    });
                    info!("ADD memory: {}", &data[..data.len().min(80)]);
                    if let Some(obj) = payload.as_object_mut() {
                        for (k, v) in &meta {
                            obj.insert(k.clone(), v.clone());
                        }
                    }
                    let point = VectorPoint {
                        id: Uuid::new_v4().to_string(),
                        vector: vector.clone(),
                        payload,
                    };
                    store.upsert(table_name, &[point]).await?;
                }
                EventType::Update => {
                    if let Some(memory_id) = &event.memory_id {
                        if !existing.iter().any(|r| r.id == *memory_id) {
                            warn!("UPDATE memory_id {memory_id} not in search results, skipping");
                            continue;
                        }
                        let new_data = event.data.as_deref().unwrap_or(fact);
                        let new_vectors = embedder.embed(&[new_data.to_string()]).await?;
                        let existing_point = existing.iter().find(|r| r.id == *memory_id);
                        let created_at = existing_point
                            .and_then(|r| r.payload.get("created_at"))
                            .cloned()
                            .unwrap_or(json!(Utc::now().to_rfc3339()));
                        let now = Utc::now().to_rfc3339();
                        let hash = md5_hex(new_data);

                        let mut payload = json!({
                            "data": new_data,
                            "hash": hash,
                            "content_hash": hash,
                            "user_id": user_id,
                            "created_at": created_at,
                            "updated_at": now,
                        });
                        info!(
                            "UPDATE memory {memory_id}: {}",
                            &new_data[..new_data.len().min(80)]
                        );
                        if let Some(obj) = payload.as_object_mut() {
                            for (k, v) in &meta {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                        let point = VectorPoint {
                            id: memory_id.clone(),
                            vector: new_vectors[0].clone(),
                            payload,
                        };
                        store.upsert(table_name, &[point]).await?;
                    } else {
                        warn!("UPDATE event missing memory_id, skipping");
                    }
                }
                EventType::Delete => {
                    if let Some(memory_id) = &event.memory_id {
                        if existing.iter().any(|r| r.id == *memory_id) {
                            store
                                .delete(table_name, std::slice::from_ref(memory_id))
                                .await?;
                        } else {
                            warn!("DELETE memory_id {memory_id} not in search results, skipping");
                        }
                    }
                }
                EventType::None => {}
            }
        }
    }
    Ok(())
}

async fn run_dedup_llm(
    existing_memories: &[serde_json::Value],
    fact: &str,
    llm: &dyn LlmProvider,
    llm_model: &str,
) -> Result<Vec<MemoryEvent>> {
    let prompt = render_prompt(
        MEMORY_UPDATE_PROMPT,
        &[
            (
                "existing_memories",
                &serde_json::to_string(existing_memories)?,
            ),
            ("new_facts", &serde_json::to_string(&[fact])?),
        ],
    );
    let messages = vec![LlmMessage {
        role: Role::User,
        content: prompt,
    }];
    let options = Some(LlmOptions {
        format_json: true,
        ..Default::default()
    });
    let response = llm.chat(llm_model, &messages, options).await?;
    let parsed: MemoryUpdateResponse =
        serde_json::from_str(&response).context("dedup response JSON parse")?;
    Ok(parsed.events)
}

/// Store content directly without LLM distillation.
pub async fn store_raw(
    content: &str,
    user_id: &str,
    metadata: &serde_json::Value,
    table_name: &str,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
) -> Result<()> {
    let vectors = embedder.embed(&[content.to_string()]).await?;
    let now = Utc::now().to_rfc3339();
    let meta = clean_metadata(metadata);
    let hash = md5_hex(content);
    let mut payload = json!({
        "data": content,
        "hash": hash,
        "content_hash": hash,
        "user_id": user_id,
        "created_at": now,
        "updated_at": serde_json::Value::Null,
    });
    debug!(table_name, "Stored raw memory");
    if let Some(obj) = payload.as_object_mut() {
        for (k, v) in &meta {
            obj.insert(k.clone(), v.clone());
        }
    }
    let point = VectorPoint {
        id: Uuid::new_v4().to_string(),
        vector: vectors[0].clone(),
        payload,
    };
    store.upsert(table_name, &[point]).await?;
    Ok(())
}

/// Full memory add pipeline: optionally distill via LLM, then store.
///
/// When `distill` is true, runs the full pipeline (extract + dedup).
/// When false, stores content directly without LLM processing.
///
/// Returns the extracted facts when distillation is on, or `None` otherwise.
#[allow(clippy::too_many_arguments)]
pub async fn do_add_memory(
    content: &str,
    user_id: &str,
    metadata: &serde_json::Value,
    distill: bool,
    table_name: &str,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    llm: &dyn LlmProvider,
    llm_model: &str,
    extraction_prompt: Option<&str>,
) -> Result<Option<Vec<String>>> {
    if !distill {
        store_raw(content, user_id, metadata, table_name, store, embedder).await?;
        return Ok(None);
    }

    let facts = extract_facts(content, llm, llm_model, extraction_prompt).await?;
    match facts {
        None => {
            warn!("Fact extraction failed, falling back to raw store");
            store_raw(content, user_id, metadata, table_name, store, embedder).await?;
            Ok(None)
        }
        Some(ref f) if f.is_empty() => {
            debug!("No facts extracted from content");
            Ok(Some(vec![]))
        }
        Some(f) => {
            dedup_and_store(
                &f, user_id, metadata, table_name, store, embedder, llm, llm_model,
            )
            .await?;
            Ok(Some(f))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_hex() {
        assert_eq!(md5_hex("hello"), "5d41402abc4b2a76b9719d911017c592");
    }

    #[test]
    fn test_clean_metadata() {
        let meta = json!({
            "data": "should be removed",
            "hash": "should be removed",
            "user_id": "should be removed",
            "created_at": "should be removed",
            "updated_at": "should be removed",
            "project": "my-project",
            "custom_key": "kept"
        });
        let cleaned = clean_metadata(&meta);
        assert!(!cleaned.contains_key("data"));
        assert!(!cleaned.contains_key("hash"));
        assert!(!cleaned.contains_key("user_id"));
        assert!(cleaned.contains_key("project"));
        assert!(cleaned.contains_key("custom_key"));
    }

    #[test]
    fn test_reserved_keys() {
        assert!(RESERVED_KEYS.contains(&"data"));
        assert!(RESERVED_KEYS.contains(&"hash"));
        assert!(RESERVED_KEYS.contains(&"user_id"));
        assert!(!RESERVED_KEYS.contains(&"source"));
    }

    #[test]
    fn test_parse_facts_response() {
        let json_str = r#"{"facts": ["fact one", "fact two"]}"#;
        let parsed: FactsResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.facts.len(), 2);
        assert_eq!(parsed.facts[0], "fact one");
    }

    #[test]
    fn test_parse_facts_response_empty() {
        let json_str = r#"{"facts": []}"#;
        let parsed: FactsResponse = serde_json::from_str(json_str).unwrap();
        assert!(parsed.facts.is_empty());
    }

    #[test]
    fn test_parse_update_response() {
        let json_str = r#"{"events": [{"type": "ADD", "data": "new fact"}, {"type": "NONE"}]}"#;
        let parsed: MemoryUpdateResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].event_type, EventType::Add);
        assert_eq!(parsed.events[0].data.as_deref(), Some("new fact"));
        assert_eq!(parsed.events[1].event_type, EventType::None);
    }

    #[test]
    fn test_memories_table_name() {
        assert_eq!(MEMORIES_TABLE, "mindojo_memories");
    }

    #[test]
    fn test_event_type_serde_roundtrip() {
        let event = MemoryEvent {
            event_type: EventType::Add,
            data: Some("test".into()),
            memory_id: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains(r#""type":"ADD""#));
        let parsed: MemoryEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.event_type, EventType::Add);
    }

    #[test]
    fn test_validate_facts_truncates_long() {
        let long_fact = "x".repeat(3000);
        let result = validate_facts(vec![long_fact]);
        assert_eq!(result.len(), 1);
        assert!(result[0].len() <= MAX_FACT_LENGTH + 3); // +3 for "..."
        assert!(result[0].ends_with("..."));
    }

    #[test]
    fn test_validate_facts_caps_count() {
        let facts: Vec<String> = (0..60).map(|i| format!("fact {i}")).collect();
        let result = validate_facts(facts);
        assert_eq!(result.len(), MAX_FACTS_PER_EXTRACTION);
    }

    #[test]
    fn test_validate_facts_passes_normal() {
        let facts = vec!["short fact".to_string()];
        let result = validate_facts(facts.clone());
        assert_eq!(result, facts);
    }
}
