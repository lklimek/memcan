//! MCP-driven code indexing from pre-read file contents.
//!
//! Takes file contents provided by the client, extracts symbols,
//! generates LLM descriptions, embeds, and upserts into vector storage.

use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use tracing::{info, warn};

use crate::error::Result;
use crate::indexing::code::{
    BATCH_SIZE, chunk_fallback, content_hash, context_line, ext_to_lang, extract_symbols_regex,
    flush_batch, generate_description, point_id,
};
use crate::pipeline::CODE_TABLE;
use crate::traits::{EmbeddingProvider, LlmProvider, TableSchema, VectorPoint, VectorStore};

/// A source file provided by the client (already read).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CodeFileInput {
    /// Relative file path (e.g. "src/main.rs").
    pub path: String,
    /// Full file content.
    pub content: String,
}

/// Parameters for MCP-driven code indexing.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct IndexCodeFilesParams {
    pub files: Vec<CodeFileInput>,
    pub project: String,
    pub tech_stack: String,
}

/// Result of indexing files.
pub struct IndexCodeFilesResult {
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Index provided file contents.
///
/// Extracts symbols (or falls back to chunking), generates LLM descriptions,
/// embeds, and upserts into the code table.
pub async fn index_code_files(
    params: &IndexCodeFilesParams,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    schema: &dyn TableSchema,
    llm: &dyn LlmProvider,
    llm_model: &str,
    embed_dims: usize,
) -> Result<IndexCodeFilesResult> {
    store.ensure_table(CODE_TABLE, embed_dims, schema).await?;

    let now = Utc::now().to_rfc3339();
    let mut total_indexed = 0usize;
    let mut total_skipped = 0usize;
    let mut total_errors = 0usize;
    let mut batch: Vec<(VectorPoint, String)> = Vec::new();

    for file in &params.files {
        let ext = file
            .path
            .rsplit('.')
            .next()
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let lang = ext_to_lang(&ext);

        let mut symbols = if let Some(l) = lang {
            extract_symbols_regex(&file.content, l)
        } else {
            Vec::new()
        };

        if symbols.is_empty() {
            symbols = chunk_fallback(&file.content);
        }

        if symbols.is_empty() {
            total_skipped += 1;
            continue;
        }

        let effective_lang = lang.unwrap_or("unknown");

        for sym in &symbols {
            let ctx_line = context_line(&file.path, effective_lang, &params.tech_stack);
            let data = format!("{}\n{}", ctx_line, sym.text);
            let chash = content_hash(&data);
            let pid = point_id(
                &params.project,
                &file.path,
                &sym.symbol_name,
                sym.start_line,
            );

            let description = match generate_description(&sym.text, llm, llm_model).await {
                Ok(desc) => {
                    let desc = desc.trim().to_string();
                    if desc.is_empty() { None } else { Some(desc) }
                }
                Err(e) => {
                    warn!(
                        symbol = %sym.symbol_name,
                        file = %file.path,
                        error = %e,
                        "LLM description generation failed"
                    );
                    None
                }
            };

            let embed_text = if let Some(ref desc) = description {
                format!("# Description: {desc}\n{data}")
            } else {
                data.clone()
            };

            let mut payload = serde_json::Map::new();
            payload.insert("data".into(), serde_json::Value::String(data));
            payload.insert(
                "collection".into(),
                serde_json::Value::String("code".into()),
            );
            payload.insert(
                "project".into(),
                serde_json::Value::String(params.project.clone()),
            );
            payload.insert(
                "file_path".into(),
                serde_json::Value::String(file.path.clone()),
            );
            payload.insert(
                "tech_stack".into(),
                serde_json::Value::String(params.tech_stack.clone()),
            );
            payload.insert(
                "chunk_type".into(),
                serde_json::Value::String(sym.chunk_type.clone()),
            );
            payload.insert(
                "symbol_name".into(),
                serde_json::Value::String(sym.symbol_name.clone()),
            );
            payload.insert(
                "start_line".into(),
                serde_json::Number::from(sym.start_line as u64).into(),
            );
            payload.insert(
                "end_line".into(),
                serde_json::Number::from(sym.end_line as u64).into(),
            );
            payload.insert("content_hash".into(), serde_json::Value::String(chash));
            if let Some(ref desc) = description {
                payload.insert(
                    "description".into(),
                    serde_json::Value::String(desc.clone()),
                );
            }
            payload.insert("indexed_at".into(), serde_json::Value::String(now.clone()));

            let point = VectorPoint {
                id: pid,
                vector: vec![0.0; embed_dims],
                payload: serde_json::Value::Object(payload),
            };

            batch.push((point, embed_text));

            if batch.len() >= BATCH_SIZE {
                match flush_batch(embedder, store, schema, CODE_TABLE, &mut batch).await {
                    Ok(n) => total_indexed += n,
                    Err(e) => {
                        warn!(error = %e, "Batch embedding failed");
                        total_errors += batch.len();
                        batch.clear();
                    }
                }
            }
        }
    }

    match flush_batch(embedder, store, schema, CODE_TABLE, &mut batch).await {
        Ok(n) => total_indexed += n,
        Err(e) => {
            warn!(error = %e, "Final batch embedding failed");
            total_errors += batch.len();
            batch.clear();
        }
    }

    info!(
        indexed = total_indexed,
        skipped = total_skipped,
        errors = total_errors,
        "Code file indexing complete"
    );

    Ok(IndexCodeFilesResult {
        indexed: total_indexed,
        skipped: total_skipped,
        errors: total_errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{LlmMessage, LlmOptions, SearchResult, VectorPoint};
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockEmbedder {
        dims: usize,
    }

    #[async_trait]
    impl EmbeddingProvider for MockEmbedder {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1; self.dims]).collect())
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
    }

    struct MockLlm {
        response: String,
    }

    #[async_trait]
    impl LlmProvider for MockLlm {
        async fn chat(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _options: Option<LlmOptions>,
        ) -> Result<String> {
            Ok(self.response.clone())
        }
        async fn context_window(&self, _model: &str) -> Option<usize> {
            Some(4096)
        }
    }

    struct MockStore {
        upserted: Mutex<Vec<(String, Vec<VectorPoint>)>>,
    }

    impl MockStore {
        fn new() -> Self {
            Self {
                upserted: Mutex::new(Vec::new()),
            }
        }

        fn upserted_count(&self) -> usize {
            self.upserted
                .lock()
                .unwrap()
                .iter()
                .map(|(_, pts)| pts.len())
                .sum()
        }
    }

    #[async_trait]
    impl VectorStore for MockStore {
        async fn ensure_table(
            &self,
            _name: &str,
            _dims: usize,
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            Ok(())
        }
        async fn upsert(
            &self,
            table: &str,
            points: &[VectorPoint],
            _schema: &dyn TableSchema,
        ) -> Result<()> {
            self.upserted
                .lock()
                .unwrap()
                .push((table.to_string(), points.to_vec()));
            Ok(())
        }
        async fn search(
            &self,
            _table: &str,
            _vector: &[f32],
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
        async fn scroll(
            &self,
            _table: &str,
            _filter: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
        async fn count(&self, _table: &str, _filter: Option<&str>) -> Result<usize> {
            Ok(0)
        }
        async fn delete(&self, _table: &str, _ids: &[String]) -> Result<()> {
            Ok(())
        }
        async fn delete_by_filter(&self, _table: &str, _filter: &str) -> Result<usize> {
            Ok(0)
        }
        async fn get(&self, _table: &str, _ids: &[String]) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn test_index_code_files_empty() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "A function".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![],
            project: "test-proj".into(),
            tech_stack: "rust".into(),
        };

        let result = index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        assert_eq!(result.indexed, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(result.errors, 0);
    }

    #[tokio::test]
    async fn test_index_code_files_rust_file() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "Main entry point".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![CodeFileInput {
                path: "src/main.rs".into(),
                content: "pub fn main() {\n    println!(\"hello\");\n}\n".into(),
            }],
            project: "test-proj".into(),
            tech_stack: "rust".into(),
        };

        let result = index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        assert_eq!(result.indexed, 1);
        assert_eq!(result.errors, 0);
        assert_eq!(store.upserted_count(), 1);
    }

    #[tokio::test]
    async fn test_index_code_files_unknown_extension_falls_back() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "A chunk".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![CodeFileInput {
                path: "config.yaml".into(),
                content: "key: value\nother: thing\n".into(),
            }],
            project: "test-proj".into(),
            tech_stack: "generic".into(),
        };

        let result = index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        assert_eq!(result.indexed, 1);
    }

    #[tokio::test]
    async fn test_index_code_files_empty_content_skipped() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![CodeFileInput {
                path: "empty.rs".into(),
                content: "".into(),
            }],
            project: "test-proj".into(),
            tech_stack: "rust".into(),
        };

        let result = index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        assert_eq!(result.indexed, 0);
        assert_eq!(result.skipped, 1);
    }

    #[tokio::test]
    async fn test_index_code_files_multiple_symbols() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "A symbol".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![CodeFileInput {
                path: "lib.rs".into(),
                content: "pub fn foo() {}\n\npub fn bar() {}\n".into(),
            }],
            project: "test-proj".into(),
            tech_stack: "rust".into(),
        };

        let result = index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        assert_eq!(result.indexed, 2);
    }

    #[tokio::test]
    async fn test_index_code_files_payload_fields() {
        let store = MockStore::new();
        let embedder = MockEmbedder { dims: 3 };
        let llm = MockLlm {
            response: "A function".into(),
        };
        let schema = crate::traits::MinimalTableSchema;

        let params = IndexCodeFilesParams {
            files: vec![CodeFileInput {
                path: "src/lib.rs".into(),
                content: "pub fn hello() {}\n".into(),
            }],
            project: "my-proj".into(),
            tech_stack: "axum".into(),
        };

        index_code_files(&params, &store, &embedder, &schema, &llm, "test", 3)
            .await
            .unwrap();

        let upserted = store.upserted.lock().unwrap();
        let payload = &upserted[0].1[0].payload;
        assert_eq!(payload["project"], "my-proj");
        assert_eq!(payload["tech_stack"], "axum");
        assert_eq!(payload["file_path"], "src/lib.rs");
        assert_eq!(payload["collection"], "code");
        assert_eq!(payload["symbol_name"], "hello");
        assert!(payload.get("content_hash").is_some());
        assert!(payload.get("indexed_at").is_some());
        assert!(payload.get("description").is_some());
    }
}
