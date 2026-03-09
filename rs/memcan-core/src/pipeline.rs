//! Memory storage pipeline: extract facts via LLM, deduplicate, store in vector DB.
//!
//! The [`Pipeline`] struct encapsulates all processing state and reports progress
//! to callers via shared `PipelineProgress` state. All backend interactions go through trait
//! objects, making the pipeline storage- and LLM-agnostic.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

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
pub const MEMORIES_TABLE: &str = "memcan_memories";

/// Table name for standards documents.
pub const STANDARDS_TABLE: &str = "memcan_standards";

/// Table name for indexed code.
pub const CODE_TABLE: &str = "memcan_code";

/// Reserved payload keys that user metadata must not overwrite.
const RESERVED_KEYS: &[&str] = &["data", "hash", "user_id", "created_at", "updated_at"];

/// Max length of a single extracted fact (chars). Longer facts are truncated.
const MAX_FACT_LENGTH: usize = 2000;

/// Max number of facts returned from a single extraction.
const MAX_FACTS_PER_EXTRACTION: usize = 50;

/// Fraction of context window available for prompt content (system + user).
const CONTEXT_BUDGET_RATIO: f32 = 0.40;

/// Fallback context window when model doesn't report one.
const DEFAULT_CONTEXT_WINDOW: usize = 4096;

/// Approximate characters per token for budget estimation.
const CHARS_PER_TOKEN: usize = 4;

// INTENTIONAL(SEC-009): MD5 used for content deduplication only, not security.
// Collision risk negligible for this use case.
/// Compute MD5 hex digest of a string.
pub fn md5_hex(data: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(data.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Strip reserved keys from user-supplied metadata.
pub fn clean_metadata(metadata: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
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

/// Pipeline processing step for status tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStep {
    Queued,
    Embedding,
    ExtractingFacts,
    Deduplicating,
    Storing,
    Completed,
    CompletedDegraded,
    Failed,
}

impl PipelineStep {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Embedding => "embedding",
            Self::ExtractingFacts => "extracting_facts",
            Self::Deduplicating => "deduplicating",
            Self::Storing => "storing",
            Self::Completed => "completed",
            Self::CompletedDegraded => "completed_degraded",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for PipelineStep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Shared progress state for a pipeline operation.
#[derive(Debug)]
pub struct PipelineProgress {
    pub step: PipelineStep,
    pub warnings: Vec<String>,
    pub error: Option<String>,
    pub completed_at: Option<String>,
}

impl Default for PipelineProgress {
    fn default() -> Self {
        Self {
            step: PipelineStep::Queued,
            warnings: vec![],
            error: None,
            completed_at: None,
        }
    }
}

/// Outcome of an add_memory operation.
#[derive(Debug, Default)]
pub struct AddMemoryResult {
    pub facts: Option<Vec<String>>,
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

/// Encapsulates all processing state for the memory pipeline.
pub struct Pipeline {
    store: Arc<dyn VectorStore>,
    embedder: Arc<dyn EmbeddingProvider>,
    llm: Arc<dyn LlmProvider>,
    llm_model: String,
    table_name: String,
    distill: bool,
    extraction_prompt: Option<String>,
    progress: Arc<Mutex<PipelineProgress>>,
    context_budget: tokio::sync::OnceCell<usize>,
}

impl Pipeline {
    pub fn new(
        store: Arc<dyn VectorStore>,
        embedder: Arc<dyn EmbeddingProvider>,
        llm: Arc<dyn LlmProvider>,
        llm_model: impl Into<String>,
        table_name: impl Into<String>,
        distill: bool,
    ) -> Self {
        Self {
            store,
            embedder,
            llm,
            llm_model: llm_model.into(),
            table_name: table_name.into(),
            distill,
            extraction_prompt: None,
            progress: Arc::new(Mutex::new(PipelineProgress::default())),
            context_budget: tokio::sync::OnceCell::new(),
        }
    }

    pub fn with_extraction_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.extraction_prompt = Some(prompt.into());
        self
    }

    /// Get shared progress handle. Clone this Arc to read progress from outside.
    pub fn progress(&self) -> Arc<Mutex<PipelineProgress>> {
        Arc::clone(&self.progress)
    }

    fn start_step(&self, step: PipelineStep) {
        self.progress.lock().unwrap_or_else(|e| e.into_inner()).step = step;
    }

    fn warn(&self, msg: impl Into<String>) {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .warnings
            .push(msg.into());
    }

    /// Mark the pipeline as completed (or completed_degraded if warnings exist).
    pub fn complete(&self) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        p.step = if p.warnings.is_empty() {
            PipelineStep::Completed
        } else {
            PipelineStep::CompletedDegraded
        };
        p.completed_at = Some(Utc::now().to_rfc3339());
    }

    /// Mark the pipeline as failed with the given error.
    pub fn fail(&self, error: impl std::fmt::Display) {
        let mut p = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        let failed_step = p.step;
        p.step = PipelineStep::Failed;
        p.error = Some(format!("Failed during {failed_step}: {error}"));
        p.completed_at = Some(Utc::now().to_rfc3339());
    }

    /// Full memory add pipeline: optionally distill via LLM, then store.
    ///
    /// Returns extracted facts when distillation succeeds, `None` otherwise.
    /// Caller is responsible for calling `complete()` or `fail()` after.
    pub async fn add_memory(
        &self,
        content: &str,
        user_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<AddMemoryResult> {
        if !self.distill {
            self.store_raw(content, user_id, metadata).await?;
            return Ok(AddMemoryResult::default());
        }

        self.start_step(PipelineStep::ExtractingFacts);
        let facts = match self.extract_facts(content).await {
            Ok(facts) => facts,
            Err(e) if e.is_llm_error() => {
                warn!("fact extraction LLM call failed: {e}");
                warn!("Fact extraction failed, falling back to raw store");
                self.warn(format!(
                    "LLM fact extraction failed: {e}. Stored without distillation."
                ));
                self.store_raw(content, user_id, metadata).await?;
                return Ok(AddMemoryResult { facts: None });
            }
            Err(e) => return Err(e),
        };

        match facts {
            None => {
                warn!("Fact extraction returned unparseable response, falling back to raw store");
                self.warn(
                    "LLM returned unparseable response. Stored without distillation.".to_string(),
                );
                self.store_raw(content, user_id, metadata).await?;
                Ok(AddMemoryResult { facts: None })
            }
            Some(ref f) if f.is_empty() => {
                debug!("No facts extracted from content");
                Ok(AddMemoryResult {
                    facts: Some(vec![]),
                })
            }
            Some(f) => {
                self.start_step(PipelineStep::Deduplicating);
                self.dedup_and_store(&f, user_id, metadata).await?;
                Ok(AddMemoryResult { facts: Some(f) })
            }
        }
    }

    /// Update an existing memory's content (embed + upsert).
    pub async fn update_memory(
        &self,
        memory_id: &str,
        content: &str,
        user_id: &str,
        created_at: serde_json::Value,
        extra_payload: Option<&serde_json::Value>,
    ) -> Result<()> {
        self.start_step(PipelineStep::Embedding);
        let vectors = self
            .embedder
            .embed(std::slice::from_ref(&content.to_string()))
            .await?;

        let hash = md5_hex(content);
        let now = Utc::now().to_rfc3339();

        let mut payload = json!({
            "data": content,
            "hash": hash,
            "content_hash": hash,
            "user_id": user_id,
            "created_at": created_at,
            "updated_at": now,
        });

        if let Some(old) = extra_payload
            && let (Some(old_obj), Some(new_obj)) = (old.as_object(), payload.as_object_mut())
        {
            for (k, v) in old_obj {
                if !matches!(
                    k.as_str(),
                    "data" | "hash" | "user_id" | "created_at" | "updated_at"
                ) {
                    new_obj.insert(k.clone(), v.clone());
                }
            }
        }

        self.start_step(PipelineStep::Storing);
        let point = VectorPoint {
            id: memory_id.to_string(),
            vector: vectors[0].clone(),
            payload,
        };
        self.store.upsert(&self.table_name, &[point]).await?;
        Ok(())
    }

    async fn store_raw(
        &self,
        content: &str,
        user_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        self.start_step(PipelineStep::Embedding);
        let vectors = self.embedder.embed(&[content.to_string()]).await?;
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
        debug!(table_name = %self.table_name, "Stored raw memory");
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
        self.start_step(PipelineStep::Storing);
        self.store.upsert(&self.table_name, &[point]).await?;
        Ok(())
    }

    async fn resolve_context_budget(&self) -> usize {
        *self
            .context_budget
            .get_or_init(|| async {
                let ctx = self
                    .llm
                    .context_window(&self.llm_model)
                    .await
                    .unwrap_or(DEFAULT_CONTEXT_WINDOW);
                let budget = (ctx as f32 * CONTEXT_BUDGET_RATIO) as usize;
                info!(
                    context_window = ctx,
                    budget_tokens = budget,
                    "resolved LLM context budget"
                );
                budget
            })
            .await
    }

    fn chunk_content<'a>(
        content: &'a str,
        system_prompt: &str,
        budget_tokens: usize,
    ) -> Vec<&'a str> {
        let system_tokens = system_prompt.len() / CHARS_PER_TOKEN;
        let user_budget_tokens = budget_tokens.saturating_sub(system_tokens);
        let max_user_chars = user_budget_tokens * CHARS_PER_TOKEN;

        if max_user_chars == 0 {
            warn!("system prompt alone exceeds context budget");
            return vec![content];
        }

        if content.len() <= max_user_chars {
            return vec![content];
        }

        let mut chunks = Vec::new();
        let mut start = 0;
        while start < content.len() {
            let end = (start + max_user_chars).min(content.len());
            let end = content.floor_char_boundary(end);

            let chunk = &content[start..end];
            let split_at = if end < content.len() {
                chunk
                    .rfind("\n\n")
                    .or_else(|| chunk.rfind('\n'))
                    .map(|pos| start + pos + 1)
                    .unwrap_or(end)
            } else {
                end
            };

            let actual_chunk = content[start..split_at].trim();
            if !actual_chunk.is_empty() {
                chunks.push(actual_chunk);
            }
            start = split_at;
        }

        if chunks.len() > 1 {
            info!(
                content_len = content.len(),
                chunks = chunks.len(),
                max_chars_per_chunk = max_user_chars,
                "content exceeds context budget, splitting into chunks"
            );
        }

        chunks
    }

    /// Extract individual facts from content using the LLM.
    async fn extract_facts(&self, content: &str) -> Result<Option<Vec<String>>> {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let prompt = self
            .extraction_prompt
            .as_deref()
            .unwrap_or(FACT_EXTRACTION_PROMPT);
        let rendered = render_prompt(prompt, &[("today", &today)]);

        let budget = self.resolve_context_budget().await;
        let chunks = Self::chunk_content(content, &rendered, budget);

        let mut all_facts = Vec::new();
        for (i, &chunk) in chunks.iter().enumerate() {
            if chunks.len() > 1 {
                debug!(
                    chunk_idx = i + 1,
                    total = chunks.len(),
                    len = chunk.len(),
                    "extracting facts from chunk"
                );
            }

            let messages = vec![
                LlmMessage {
                    role: Role::System,
                    content: rendered.clone(),
                },
                LlmMessage {
                    role: Role::User,
                    content: chunk.to_string(),
                },
            ];
            let options = Some(LlmOptions {
                format_json: true,
                think: Some(false),
                ..Default::default()
            });

            match self.llm.chat(&self.llm_model, &messages, options).await {
                Ok(response) => match serde_json::from_str::<FactsResponse>(&response) {
                    Ok(parsed) => all_facts.extend(parsed.facts),
                    Err(e) if chunks.len() > 1 => {
                        warn!(chunk_idx = i + 1, "fact extraction JSON parse failed: {e}");
                    }
                    Err(e) => {
                        warn!("fact extraction JSON parse failed: {e}");
                        return Ok(None);
                    }
                },
                Err(e) if e.is_llm_error() && chunks.len() > 1 => {
                    warn!(chunk_idx = i + 1, "fact extraction LLM call failed: {e}");
                }
                Err(e) => return Err(e),
            }
        }

        if all_facts.is_empty() && chunks.len() > 1 {
            return Ok(None);
        }

        Ok(Some(validate_facts(all_facts)))
    }

    /// Deduplicate facts against existing memories and store.
    async fn dedup_and_store(
        &self,
        facts: &[String],
        user_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        let meta = clean_metadata(metadata);

        for fact in facts {
            let vectors = self.embedder.embed(std::slice::from_ref(fact)).await?;
            let vector = &vectors[0];

            let filter = format!("user_id = '{}'", user_id.replace('\'', "''"));
            let existing = self
                .store
                .search(&self.table_name, vector, Some(&filter), 5, 0)
                .await?;

            let existing_memories: Vec<serde_json::Value> = existing
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "text": r.payload.get("data").and_then(|v| v.as_str()).unwrap_or(""),
                    })
                })
                .collect();

            let events =
                match run_dedup_llm(&existing_memories, fact, self.llm.as_ref(), &self.llm_model)
                    .await
                {
                    Ok(events) => events,
                    Err(e) => {
                        warn!("dedup LLM failed, falling back to ADD: {e}");
                        self.warn(format!(
                            "Dedup LLM failed: {e}. Added without deduplication."
                        ));
                        vec![MemoryEvent {
                            event_type: EventType::Add,
                            data: Some(fact.clone()),
                            memory_id: None,
                        }]
                    }
                };

            self.start_step(PipelineStep::Storing);
            for event in events {
                match event.event_type {
                    EventType::Add => {
                        let data = event.data.as_deref().unwrap_or(fact);
                        let add_vector = if data != fact {
                            let new_vecs = self.embedder.embed(&[data.to_string()]).await?;
                            new_vecs
                                .into_iter()
                                .next()
                                .unwrap_or_else(|| vector.clone())
                        } else {
                            vector.clone()
                        };
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
                            vector: add_vector,
                            payload,
                        };
                        self.store.upsert(&self.table_name, &[point]).await?;
                    }
                    EventType::Update => {
                        if let Some(memory_id) = &event.memory_id {
                            if !existing.iter().any(|r| r.id == *memory_id) {
                                warn!(
                                    "UPDATE memory_id {memory_id} not in search results, skipping"
                                );
                                continue;
                            }
                            let new_data = event.data.as_deref().unwrap_or(fact);
                            let new_vectors = self.embedder.embed(&[new_data.to_string()]).await?;
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
                            self.store.upsert(&self.table_name, &[point]).await?;
                        } else {
                            warn!("UPDATE event missing memory_id, skipping");
                        }
                    }
                    EventType::Delete => {
                        if let Some(memory_id) = &event.memory_id {
                            if existing.iter().any(|r| r.id == *memory_id) {
                                self.store
                                    .delete(&self.table_name, std::slice::from_ref(memory_id))
                                    .await?;
                            } else {
                                warn!(
                                    "DELETE memory_id {memory_id} not in search results, skipping"
                                );
                            }
                        }
                    }
                    EventType::None => {}
                }
            }
        }
        Ok(())
    }
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
        think: Some(false),
        ..Default::default()
    });
    let response = llm.chat(llm_model, &messages, options).await?;
    let parsed: MemoryUpdateResponse =
        serde_json::from_str(&response).context("dedup response JSON parse")?;
    Ok(parsed.events)
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
        assert_eq!(MEMORIES_TABLE, "memcan_memories");
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

    #[test]
    fn test_pipeline_step_as_str() {
        assert_eq!(PipelineStep::Queued.as_str(), "queued");
        assert_eq!(PipelineStep::Embedding.as_str(), "embedding");
        assert_eq!(PipelineStep::ExtractingFacts.as_str(), "extracting_facts");
        assert_eq!(PipelineStep::Deduplicating.as_str(), "deduplicating");
        assert_eq!(PipelineStep::Storing.as_str(), "storing");
        assert_eq!(PipelineStep::Completed.as_str(), "completed");
        assert_eq!(
            PipelineStep::CompletedDegraded.as_str(),
            "completed_degraded"
        );
        assert_eq!(PipelineStep::Failed.as_str(), "failed");
    }

    #[test]
    fn test_pipeline_step_display() {
        assert_eq!(format!("{}", PipelineStep::Embedding), "embedding");
        assert_eq!(format!("{}", PipelineStep::Failed), "failed");
    }

    #[test]
    fn test_pipeline_progress_default() {
        let p = PipelineProgress::default();
        assert_eq!(p.step, PipelineStep::Queued);
        assert!(p.warnings.is_empty());
        assert!(p.error.is_none());
        assert!(p.completed_at.is_none());
    }

    #[test]
    fn test_chunk_content_fits() {
        let content = "short content";
        let system = "system prompt";
        let budget = 1000;
        let chunks = Pipeline::chunk_content(content, system, budget);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short content");
    }

    #[test]
    fn test_chunk_content_splits() {
        let content = "a".repeat(5000);
        let system = "sys";
        let budget = 500; // 500 tokens = ~2000 chars, minus ~1 token for sys = ~1996 chars
        let chunks = Pipeline::chunk_content(&content, system, budget);
        assert!(
            chunks.len() > 1,
            "expected multiple chunks, got {}",
            chunks.len()
        );
        for &chunk in &chunks {
            assert!(chunk.len() <= 2000, "chunk too large: {}", chunk.len());
        }
    }

    #[test]
    fn test_chunk_content_paragraph_boundary() {
        let para1 = "a".repeat(100);
        let para2 = "b".repeat(100);
        let content = format!("{}\n\n{}", para1, para2);
        let system = "";
        let budget = 40; // 40 tokens = 160 chars, fits para1 (100) but not both (202)
        let chunks = Pipeline::chunk_content(&content, system, budget);
        assert_eq!(chunks.len(), 2, "expected split at paragraph boundary");
        assert!(chunks[0].contains("aaa"));
        assert!(chunks[1].contains("bbb"));
    }

    #[test]
    fn test_chunk_content_zero_budget() {
        let content = "some content";
        let system = "a".repeat(5000);
        let budget = 100; // 100 tokens = 400 chars, system is 5000 chars = 1250 tokens
        let chunks = Pipeline::chunk_content(content, &system, budget);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "some content");
    }

    #[test]
    fn test_chunk_content_multibyte() {
        let content = "🎉".repeat(500); // 2000 bytes
        let system = "";
        let budget = 200; // 200 tokens = 800 chars max
        let chunks = Pipeline::chunk_content(&content, system, budget);
        assert!(!chunks.is_empty());
        for &chunk in &chunks {
            assert!(!chunk.is_empty());
            let _ = chunk.chars().count();
        }
    }
}
