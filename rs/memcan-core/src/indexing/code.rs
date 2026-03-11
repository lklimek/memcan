//! Index source code into vector storage.
//!
//! Walks a project directory, extracts symbols via regex patterns per language,
//! falls back to line-based chunking, then embeds and upserts into the vector store.
//! Supports incremental indexing via content hashing and stale-file cleanup.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use crate::error::{MemcanError, Result, ResultExt};
use crate::pipeline::CODE_TABLE;
use crate::traits::{EmbeddingProvider, VectorPoint, VectorStore};

const UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa3, 0xe1, 0xf8, 0xc0, 0x7b, 0x2d, 0x4e, 0x5a, 0x9f, 0x1c, 0x6d, 0x8b, 0x0e, 0x3a, 0x5c, 0x7f,
]);

const CHUNK_LINES: usize = 100;
const BATCH_SIZE: usize = 20;
const MAX_WALK_DEPTH: usize = 50;

const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    "vendor",
    ".tox",
];

/// Parameters for indexing a code project.
pub struct IndexCodeParams {
    pub project_dir: PathBuf,
    pub project: String,
    pub tech_stack: String,
    pub max_file_size: u64,
}

/// Result of an indexing operation.
pub struct IndexCodeResult {
    pub upserted: usize,
    pub unchanged: usize,
    pub errors: usize,
    pub deleted: usize,
}

struct Symbol {
    text: String,
    symbol_name: String,
    start_line: usize,
    end_line: usize,
    chunk_type: String,
}

struct LangPatterns {
    patterns: Vec<(&'static str, Regex)>,
}

fn lang_extensions() -> &'static HashMap<&'static str, &'static [&'static str]> {
    static INSTANCE: OnceLock<HashMap<&str, &[&str]>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("rust", &[".rs"][..]);
        m.insert("python", &[".py"][..]);
        m.insert("go", &[".go"][..]);
        m.insert("typescript", &[".ts", ".tsx"][..]);
        m
    })
}

fn ext_to_lang(ext: &str) -> Option<&'static str> {
    for (lang, exts) in lang_extensions() {
        if exts.contains(&ext) {
            return Some(lang);
        }
    }
    None
}

fn all_extensions() -> &'static HashSet<&'static str> {
    static INSTANCE: OnceLock<HashSet<&str>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        lang_extensions()
            .values()
            .flat_map(|exts| exts.iter().copied())
            .collect()
    })
}

fn should_skip(rel_path: &Path) -> bool {
    rel_path
        .components()
        .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

fn get_lang_patterns(lang: &str) -> Option<&'static LangPatterns> {
    static RUST: OnceLock<LangPatterns> = OnceLock::new();
    static PYTHON: OnceLock<LangPatterns> = OnceLock::new();
    static GO: OnceLock<LangPatterns> = OnceLock::new();
    static TYPESCRIPT: OnceLock<LangPatterns> = OnceLock::new();

    match lang {
        "rust" => Some(RUST.get_or_init(|| LangPatterns {
            patterns: vec![
                (
                    "function_item",
                    Regex::new(r"^(?:pub(?:\(crate\))?\s+)?(?:async\s+)?fn\s+(\w+)").unwrap(),
                ),
                (
                    "struct_item",
                    Regex::new(r"^(?:pub(?:\(crate\))?\s+)?struct\s+(\w+)").unwrap(),
                ),
                (
                    "enum_item",
                    Regex::new(r"^(?:pub(?:\(crate\))?\s+)?enum\s+(\w+)").unwrap(),
                ),
                (
                    "trait_item",
                    Regex::new(r"^(?:pub(?:\(crate\))?\s+)?trait\s+(\w+)").unwrap(),
                ),
                (
                    "impl_item",
                    Regex::new(r"^impl(?:<[^>]*>)?\s+(\w+)").unwrap(),
                ),
                (
                    "mod_item",
                    Regex::new(r"^(?:pub(?:\(crate\))?\s+)?mod\s+(\w+)").unwrap(),
                ),
            ],
        })),
        "python" => Some(PYTHON.get_or_init(|| LangPatterns {
            patterns: vec![
                (
                    "function_definition",
                    Regex::new(r"^(?:async\s+)?def\s+(\w+)").unwrap(),
                ),
                ("class_definition", Regex::new(r"^class\s+(\w+)").unwrap()),
            ],
        })),
        "go" => Some(GO.get_or_init(|| LangPatterns {
            patterns: vec![
                (
                    "function_declaration",
                    Regex::new(r"^func\s+(\w+)").unwrap(),
                ),
                (
                    "method_declaration",
                    Regex::new(r"^func\s+\([^)]+\)\s+(\w+)").unwrap(),
                ),
                ("type_declaration", Regex::new(r"^type\s+(\w+)").unwrap()),
            ],
        })),
        "typescript" => Some(TYPESCRIPT.get_or_init(|| LangPatterns {
            patterns: vec![
                (
                    "function_declaration",
                    Regex::new(r"^(?:export\s+)?(?:async\s+)?function\s+(\w+)").unwrap(),
                ),
                (
                    "class_declaration",
                    Regex::new(r"^(?:export\s+)?(?:abstract\s+)?class\s+(\w+)").unwrap(),
                ),
                (
                    "interface_declaration",
                    Regex::new(r"^(?:export\s+)?interface\s+(\w+)").unwrap(),
                ),
                (
                    "type_alias_declaration",
                    Regex::new(r"^(?:export\s+)?type\s+(\w+)").unwrap(),
                ),
            ],
        })),
        _ => None,
    }
}

/// Extract top-level symbols from source code using regex patterns.
pub fn extract_symbols(source: &str, lang: &str) -> Vec<(String, String, usize, usize, String)> {
    let symbols = extract_symbols_regex(source, lang);
    symbols
        .into_iter()
        .map(|s| {
            (
                s.text,
                s.symbol_name,
                s.start_line,
                s.end_line,
                s.chunk_type,
            )
        })
        .collect()
}

fn extract_symbols_regex(source: &str, lang: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let Some(lang_patterns) = get_lang_patterns(lang) else {
        return Vec::new();
    };

    let mut item_starts: Vec<(usize, String, String)> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_top_level = if lang == "python" {
            !line.starts_with(' ') && !line.starts_with('\t')
        } else {
            let indent = line.len() - trimmed.len();
            indent == 0
        };

        if !is_top_level {
            continue;
        }

        for (item_type, re) in &lang_patterns.patterns {
            if let Some(caps) = re.captures(trimmed) {
                let name = caps
                    .get(1)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_else(|| "<anonymous>".to_string());
                item_starts.push((i, name, item_type.to_string()));
                break;
            }
        }
    }

    if item_starts.is_empty() {
        return Vec::new();
    }

    let mut symbols = Vec::new();
    for (idx, (start_line, name, chunk_type)) in item_starts.iter().enumerate() {
        let end_line = if idx + 1 < item_starts.len() {
            item_starts[idx + 1].0.saturating_sub(1)
        } else {
            lines.len() - 1
        };

        let mut actual_end = end_line;
        while actual_end > *start_line && lines[actual_end].trim().is_empty() {
            actual_end -= 1;
        }

        let text: String = lines[*start_line..=actual_end].join("\n");
        if !text.trim().is_empty() {
            symbols.push(Symbol {
                text,
                symbol_name: name.clone(),
                start_line: start_line + 1,
                end_line: actual_end + 1,
                chunk_type: chunk_type.clone(),
            });
        }
    }

    symbols
}

fn chunk_fallback(source: &str) -> Vec<Symbol> {
    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();

    for (chunk_idx, chunk_start) in (0..lines.len()).step_by(CHUNK_LINES).enumerate() {
        let chunk_end = (chunk_start + CHUNK_LINES).min(lines.len());
        let text: String = lines[chunk_start..chunk_end].join("\n");
        if text.trim().is_empty() {
            continue;
        }
        chunks.push(Symbol {
            text,
            symbol_name: format!("chunk_{}", chunk_idx),
            start_line: chunk_start + 1,
            end_line: chunk_end,
            chunk_type: "chunk".to_string(),
        });
    }

    chunks
}

fn git_short_hash(project_dir: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(project_dir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn point_id(project: &str, file_path: &str, symbol_name: &str, start_line: usize) -> String {
    let key = format!("{}:{}:{}:{}", project, file_path, symbol_name, start_line);
    Uuid::new_v5(&UUID_NAMESPACE, key.as_bytes()).to_string()
}

fn context_line(file_path: &str, lang: &str, tech_stack: &str) -> String {
    format!(
        "# file: {} | lang: {} | stack: {}",
        file_path, lang, tech_stack
    )
}

fn collect_files(project_dir: &Path) -> Vec<PathBuf> {
    let valid_exts = all_extensions();
    let mut files = Vec::new();

    fn walk(
        dir: &Path,
        project_dir: &Path,
        valid_exts: &HashSet<&str>,
        files: &mut Vec<PathBuf>,
        depth: usize,
    ) {
        if depth > MAX_WALK_DEPTH {
            warn!(
                dir = %dir.display(),
                max_depth = MAX_WALK_DEPTH,
                "Skipping directory: maximum recursion depth exceeded"
            );
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        let mut entries_vec: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries_vec.sort_by_key(|a| a.file_name());

        for entry in entries_vec {
            let path = entry.path();
            let rel = path.strip_prefix(project_dir).unwrap_or(&path);

            if should_skip(rel) {
                continue;
            }

            if path.is_dir() {
                walk(&path, project_dir, valid_exts, files, depth + 1);
            } else if path.is_file()
                && let Some(ext) = path.extension()
            {
                let ext_str = format!(".{}", ext.to_string_lossy());
                if valid_exts.contains(ext_str.as_str()) {
                    files.push(path);
                }
            }
        }
    }

    walk(project_dir, project_dir, valid_exts, &mut files, 0);
    files
}

async fn flush_batch(
    embedder: &dyn EmbeddingProvider,
    store: &dyn VectorStore,
    table: &str,
    batch: &mut Vec<(VectorPoint, String)>,
) -> Result<usize> {
    if batch.is_empty() {
        return Ok(0);
    }

    let texts: Vec<String> = batch.iter().map(|(_, text)| text.clone()).collect();
    let vectors = embedder.embed(&texts).await?;

    let points: Vec<VectorPoint> = batch
        .drain(..)
        .zip(vectors)
        .map(|((mut point, _), vec)| {
            point.vector = vec;
            point
        })
        .collect();

    let count = points.len();
    store.upsert(table, &points).await?;
    Ok(count)
}

/// Drop all indexed code for a given project.
pub async fn drop_code(project: &str, store: &dyn VectorStore, embed_dims: usize) -> Result<usize> {
    store.ensure_table(CODE_TABLE, embed_dims).await?;
    let filter = format!("project = '{}'", project.replace('\'', "''"));
    let deleted = store.delete_by_filter(CODE_TABLE, &filter).await?;
    info!(deleted, project, "Dropped indexed data");
    Ok(deleted)
}

/// Index source code from a project directory.
///
/// Walks the directory tree, extracts symbols or falls back to line-based chunks,
/// embeds each chunk, and upserts into the vector store. Performs incremental
/// updates via content hashing and cleans up stale entries for deleted files.
pub async fn index_code(
    params: &IndexCodeParams,
    store: &dyn VectorStore,
    embedder: &dyn EmbeddingProvider,
    embed_dims: usize,
) -> Result<IndexCodeResult> {
    let project_dir = params.project_dir.canonicalize().with_context(|| {
        format!(
            "Project directory does not exist: {}",
            params.project_dir.display()
        )
    })?;

    if !project_dir.is_dir() {
        return Err(MemcanError::Other(format!(
            "Not a directory: {}",
            project_dir.display()
        )));
    }

    store.ensure_table(CODE_TABLE, embed_dims).await?;

    let git_hash = git_short_hash(&project_dir);
    let now = Utc::now().to_rfc3339();

    let project_filter = format!("project = '{}'", params.project.replace('\'', "''"));
    let existing_records = store
        .scroll(CODE_TABLE, Some(&project_filter), 100_000, 0)
        .await?;
    let mut existing_hashes: HashMap<String, (String, String)> = HashMap::new();
    for record in &existing_records {
        let ch = record
            .payload
            .get("content_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let fp = record
            .payload
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        existing_hashes.insert(record.id.clone(), (ch, fp));
    }
    let indexed_file_paths: HashSet<String> =
        existing_hashes.values().map(|(_, fp)| fp.clone()).collect();

    let files = collect_files(&project_dir);
    info!(count = files.len(), dir = %project_dir.display(), "Found source files");

    let mut current_file_paths: HashSet<String> = HashSet::new();
    let mut total_upserted = 0usize;
    let mut total_skipped = 0usize;
    let mut total_errors = 0usize;
    let mut batch: Vec<(VectorPoint, String)> = Vec::new();

    for file_path in &files {
        let rel_path = file_path
            .strip_prefix(&project_dir)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();
        current_file_paths.insert(rel_path.clone());

        let lang = file_path
            .extension()
            .and_then(|e| ext_to_lang(&format!(".{}", e.to_string_lossy())));

        if file_path.is_symlink() {
            warn!(path = %rel_path, "Skipping symlink");
            continue;
        }

        let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        if file_size > params.max_file_size {
            warn!(path = %rel_path, size = file_size, limit = params.max_file_size, "Skipping oversized file");
            continue;
        }

        let source_text = match std::fs::read_to_string(file_path) {
            Ok(s) => s,
            Err(e) => {
                warn!(path = %rel_path, error = %e, "Failed to read file");
                total_errors += 1;
                continue;
            }
        };

        let mut symbols = if let Some(l) = lang {
            extract_symbols_regex(&source_text, l)
        } else {
            Vec::new()
        };

        if symbols.is_empty() {
            symbols = chunk_fallback(&source_text);
        }

        if symbols.is_empty() {
            continue;
        }

        let effective_lang = lang.unwrap_or("unknown");

        for sym in &symbols {
            let ctx_line = context_line(&rel_path, effective_lang, &params.tech_stack);
            let data = format!("{}\n{}", ctx_line, sym.text);
            let chash = content_hash(&data);
            let pid = point_id(&params.project, &rel_path, &sym.symbol_name, sym.start_line);

            if let Some((existing_hash, _)) = existing_hashes.get(&pid)
                && *existing_hash == chash
            {
                total_skipped += 1;
                continue;
            }

            let mut payload = serde_json::Map::new();
            payload.insert("data".into(), serde_json::Value::String(data.clone()));
            payload.insert(
                "project".into(),
                serde_json::Value::String(params.project.clone()),
            );
            payload.insert(
                "file_path".into(),
                serde_json::Value::String(rel_path.clone()),
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
                serde_json::Value::Number(serde_json::Number::from(sym.start_line as u64)),
            );
            payload.insert(
                "end_line".into(),
                serde_json::Value::Number(serde_json::Number::from(sym.end_line as u64)),
            );
            payload.insert("content_hash".into(), serde_json::Value::String(chash));
            payload.insert(
                "git_hash".into(),
                serde_json::Value::String(git_hash.clone()),
            );
            payload.insert("indexed_at".into(), serde_json::Value::String(now.clone()));

            let point = VectorPoint {
                id: pid,
                vector: vec![0.0; embed_dims],
                payload: serde_json::Value::Object(payload),
            };

            batch.push((point, data));

            if batch.len() >= BATCH_SIZE {
                match flush_batch(embedder, store, CODE_TABLE, &mut batch).await {
                    Ok(n) => {
                        total_upserted += n;
                        info!(upserted = total_upserted, "Progress");
                    }
                    Err(e) => {
                        warn!(error = %e, "Batch embedding failed");
                        total_errors += batch.len();
                        batch.clear();
                    }
                }
            }
        }
    }

    match flush_batch(embedder, store, CODE_TABLE, &mut batch).await {
        Ok(n) => total_upserted += n,
        Err(e) => {
            warn!(error = %e, "Final batch embedding failed");
            total_errors += batch.len();
            batch.clear();
        }
    }

    let removed_files: Vec<String> = indexed_file_paths
        .difference(&current_file_paths)
        .cloned()
        .collect();
    let mut total_deleted = 0usize;
    for fp in &removed_files {
        let filter = format!(
            "project = '{}' AND file_path = '{}'",
            params.project.replace('\'', "''"),
            fp.replace('\'', "''")
        );
        match store.delete_by_filter(CODE_TABLE, &filter).await {
            Ok(n) => total_deleted += n,
            Err(e) => warn!(file = %fp, error = %e, "Failed to delete removed file points"),
        }
    }

    info!(
        upserted = total_upserted,
        unchanged = total_skipped,
        errors = total_errors,
        deleted = total_deleted,
        "Indexing complete"
    );

    Ok(IndexCodeResult {
        upserted: total_upserted,
        unchanged: total_skipped,
        errors: total_errors,
        deleted: total_deleted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ext_to_lang() {
        assert_eq!(ext_to_lang(".rs"), Some("rust"));
        assert_eq!(ext_to_lang(".py"), Some("python"));
        assert_eq!(ext_to_lang(".go"), Some("go"));
        assert_eq!(ext_to_lang(".ts"), Some("typescript"));
        assert_eq!(ext_to_lang(".tsx"), Some("typescript"));
        assert_eq!(ext_to_lang(".java"), None);
    }

    #[test]
    fn test_should_skip() {
        assert!(should_skip(Path::new("node_modules/foo.js")));
        assert!(should_skip(Path::new("src/.git/config")));
        assert!(should_skip(Path::new("target/debug/build")));
        assert!(!should_skip(Path::new("src/main.rs")));
        assert!(!should_skip(Path::new("lib/utils.py")));
    }

    #[test]
    fn test_extract_symbols_rust_fn() {
        let source = "pub fn hello() {\n    println!(\"hi\");\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].1, "hello");
        assert_eq!(symbols[0].4, "function_item");
    }

    #[test]
    fn test_extract_symbols_rust_struct_and_impl() {
        let source = "pub struct Foo {\n    bar: i32,\n}\n\nimpl Foo {\n    fn new() -> Self {\n        Self { bar: 0 }\n    }\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].1, "Foo");
        assert_eq!(symbols[0].4, "struct_item");
        assert_eq!(symbols[1].1, "Foo");
        assert_eq!(symbols[1].4, "impl_item");
    }

    #[test]
    fn test_extract_symbols_python() {
        let source = "def greet(name):\n    print(f'Hello {name}')\n\nclass Greeter:\n    pass\n";
        let symbols = extract_symbols(source, "python");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].1, "greet");
        assert_eq!(symbols[0].4, "function_definition");
        assert_eq!(symbols[1].1, "Greeter");
        assert_eq!(symbols[1].4, "class_definition");
    }

    #[test]
    fn test_extract_symbols_go() {
        let source = "func main() {\n    fmt.Println(\"hello\")\n}\n\ntype Config struct {\n    Name string\n}\n";
        let symbols = extract_symbols(source, "go");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].1, "main");
        assert_eq!(symbols[1].1, "Config");
    }

    #[test]
    fn test_extract_symbols_typescript() {
        let source = "export function hello(): void {\n    console.log('hi');\n}\n\nexport interface Props {\n    name: string;\n}\n";
        let symbols = extract_symbols(source, "typescript");
        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0].1, "hello");
        assert_eq!(symbols[0].4, "function_declaration");
        assert_eq!(symbols[1].1, "Props");
        assert_eq!(symbols[1].4, "interface_declaration");
    }

    #[test]
    fn test_extract_symbols_unknown_lang() {
        let source = "some code here";
        let symbols = extract_symbols(source, "java");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_extract_symbols_empty_source() {
        let symbols = extract_symbols("", "rust");
        assert!(symbols.is_empty());
    }

    #[test]
    fn test_chunk_fallback_small() {
        let source = "line1\nline2\nline3\n";
        let chunks = chunk_fallback(source);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_name, "chunk_0");
        assert_eq!(chunks[0].chunk_type, "chunk");
    }

    #[test]
    fn test_chunk_fallback_empty() {
        let chunks = chunk_fallback("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash("hello world");
        let h2 = content_hash("hello world");
        assert_eq!(h1, h2);
        let h3 = content_hash("different");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_point_id_deterministic() {
        let id1 = point_id("proj", "src/main.rs", "main", 1);
        let id2 = point_id("proj", "src/main.rs", "main", 1);
        assert_eq!(id1, id2);
        let id3 = point_id("proj", "src/main.rs", "main", 2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_context_line_format() {
        let line = context_line("src/main.rs", "rust", "actix-web");
        assert_eq!(line, "# file: src/main.rs | lang: rust | stack: actix-web");
    }

    #[test]
    fn test_collect_files_skips_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let node_modules = dir.path().join("node_modules");
        std::fs::create_dir(&node_modules).unwrap();
        std::fs::write(node_modules.join("foo.rs"), "fn test() {}").unwrap();

        let src = dir.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("main.rs"), "fn main() {}").unwrap();

        let files = collect_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("main.rs"));
    }

    #[test]
    fn test_collect_files_filters_extensions() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(dir.path().join("readme.md"), "# Hello").unwrap();
        std::fs::write(dir.path().join("app.py"), "def main(): pass").unwrap();

        let files = collect_files(dir.path());
        assert_eq!(files.len(), 2);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert!(names.contains(&"main.rs".to_string()));
        assert!(names.contains(&"app.py".to_string()));
    }

    #[test]
    fn test_extract_symbols_ignores_indented() {
        let source = "fn top_level() {\n    fn inner() {}\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].1, "top_level");
    }

    #[test]
    fn test_extract_symbols_rust_async_fn() {
        let source = "pub async fn serve() {\n    todo!()\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].1, "serve");
        assert_eq!(symbols[0].4, "function_item");
    }

    #[test]
    fn test_extract_symbols_rust_enum() {
        let source = "pub enum Color {\n    Red,\n    Blue,\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].1, "Color");
        assert_eq!(symbols[0].4, "enum_item");
    }

    #[test]
    fn test_extract_symbols_rust_trait() {
        let source = "pub trait Display {\n    fn fmt(&self);\n}\n";
        let symbols = extract_symbols(source, "rust");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].1, "Display");
        assert_eq!(symbols[0].4, "trait_item");
    }
}
