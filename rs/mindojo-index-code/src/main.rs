//! mindojo-index-code — Index source code into vector store.
//!
//! Parses supported languages (Rust, Python, Go, TypeScript) using simple regex
//! patterns to detect top-level items, embeds them, and upserts into the code table.
//! Falls back to 100-line chunks when patterns don't match.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use clap::Parser;
use regex::Regex;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use mindojo_core::config::Settings;
use mindojo_core::embed::FastEmbedProvider;
use mindojo_core::error::{MindojoError, Result as MindojoResult, ResultExt};
use mindojo_core::lancedb_store::LanceDbStore;
use mindojo_core::pipeline::CODE_TABLE;
use mindojo_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

#[derive(Parser)]
#[command(name = "mindojo-index-code")]
#[command(about = "Index source code into vector store with symbol extraction")]
struct Cli {
    /// Root directory of the project to index
    project_dir: PathBuf,

    /// Project name for payload tagging
    #[arg(long)]
    project: String,

    /// Tech stack label
    #[arg(long)]
    tech_stack: Option<String>,

    /// Drop all indexed data for this project
    #[arg(long)]
    drop: bool,

    /// Enable debug logging
    #[arg(long)]
    verbose: bool,

    /// Skip files larger than this many bytes (default: 1 MB)
    #[arg(long, default_value = "1048576")]
    max_file_size: u64,
}

/// UUID namespace for deterministic point IDs.
const UUID_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa3, 0xe1, 0xf8, 0xc0, 0x7b, 0x2d, 0x4e, 0x5a, 0x9f, 0x1c, 0x6d, 0x8b, 0x0e, 0x3a, 0x5c, 0x7f,
]);

/// Number of lines per chunk in fallback mode.
const CHUNK_LINES: usize = 100;

/// Batch size for embedding requests.
const BATCH_SIZE: usize = 20;

/// Maximum recursion depth for directory walking.
const MAX_WALK_DEPTH: usize = 50;

/// Directories to skip during file collection.
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

/// Language extension mappings (compiled once).
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

/// Map file extension to language name.
fn ext_to_lang(ext: &str) -> Option<&'static str> {
    for (lang, exts) in lang_extensions() {
        if exts.contains(&ext) {
            return Some(lang);
        }
    }
    None
}

/// All supported file extensions (compiled once).
fn all_extensions() -> &'static HashSet<&'static str> {
    static INSTANCE: OnceLock<HashSet<&str>> = OnceLock::new();
    INSTANCE.get_or_init(|| {
        lang_extensions()
            .values()
            .flat_map(|exts| exts.iter().copied())
            .collect()
    })
}

/// Check if a path component is in the skip list.
fn should_skip(rel_path: &Path) -> bool {
    rel_path
        .components()
        .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

/// An extracted code symbol or chunk.
struct Symbol {
    text: String,
    symbol_name: String,
    start_line: usize,
    end_line: usize,
    chunk_type: String,
}

/// Pre-compiled regex patterns for a language.
struct LangPatterns {
    patterns: Vec<(&'static str, Regex)>,
}

/// Get pre-compiled regex patterns for symbol extraction (compiled once per language).
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

/// Regex patterns for detecting top-level items by language.
fn extract_symbols_regex(source: &str, lang: &str, _file_path: &str) -> Vec<Symbol> {
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

/// Fallback: split source into fixed-size line chunks.
fn chunk_fallback(source: &str, _file_path: &str) -> Vec<Symbol> {
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

/// Get the short git hash for a project directory.
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

/// Compute SHA256 hex digest of content.
fn content_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generate a deterministic UUID v5 point ID.
fn point_id(project: &str, file_path: &str, symbol_name: &str, start_line: usize) -> String {
    let key = format!("{}:{}:{}:{}", project, file_path, symbol_name, start_line);
    Uuid::new_v5(&UUID_NAMESPACE, key.as_bytes()).to_string()
}

/// Build a context line for embedding.
fn context_line(file_path: &str, lang: &str, tech_stack: &str) -> String {
    format!(
        "# file: {} | lang: {} | stack: {}",
        file_path, lang, tech_stack
    )
}

/// Collect all source files in the project directory (depth-limited).
// TODO: Replace with `walkdir` crate for robustness and configurability.
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

    let settings = Settings::load()?;
    let embedder = FastEmbedProvider::from_settings(&settings)?;
    let store = LanceDbStore::open(&settings.lancedb_path).await?;

    let table = CODE_TABLE;

    // Handle --drop mode
    if cli.drop {
        store.ensure_table(table, settings.embed_dims).await?;
        let filter = format!(
            "JSON_EXTRACT(payload, '$.project') = '{}'",
            cli.project.replace('\'', "''")
        );
        let deleted = store.delete_by_filter(table, &filter).await?;
        info!(deleted, project = %cli.project, "Dropped indexed data");
        return Ok(());
    }

    let tech_stack = cli.tech_stack.as_deref().ok_or_else(|| {
        MindojoError::Other("--tech-stack is required unless --drop is specified".into())
    })?;

    let project_dir = cli.project_dir.canonicalize().with_context(|| {
        format!(
            "Project directory does not exist: {}",
            cli.project_dir.display()
        )
    })?;

    if !project_dir.is_dir() {
        return Err(MindojoError::Other(format!(
            "Not a directory: {}",
            project_dir.display()
        )));
    }

    store.ensure_table(table, settings.embed_dims).await?;

    let git_hash = git_short_hash(&project_dir);
    let now = Utc::now().to_rfc3339();

    // Get existing indexed data for incremental re-indexing.
    // TODO: Optimize to query only content_hash + file_path columns instead of
    // loading full records, once LanceDB supports column projection in scroll().
    let project_filter = format!(
        "JSON_EXTRACT(payload, '$.project') = '{}'",
        cli.project.replace('\'', "''")
    );
    let existing_records = store.scroll(table, Some(&project_filter), 100_000).await?;
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

        // Skip symlinks
        if file_path.is_symlink() {
            warn!(path = %rel_path, "Skipping symlink");
            continue;
        }

        // Skip oversized files
        let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
        if file_size > cli.max_file_size {
            warn!(
                path = %rel_path,
                size = file_size,
                limit = cli.max_file_size,
                "Skipping oversized file"
            );
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

        // Extract symbols or fall back to chunks
        let mut symbols = if let Some(l) = lang {
            extract_symbols_regex(&source_text, l, &rel_path)
        } else {
            Vec::new()
        };

        if symbols.is_empty() {
            symbols = chunk_fallback(&source_text, &rel_path);
        }

        if symbols.is_empty() {
            continue;
        }

        let effective_lang = lang.unwrap_or("unknown");

        for sym in &symbols {
            let ctx = context_line(&rel_path, effective_lang, tech_stack);
            let data = format!("{}\n{}", ctx, sym.text);
            let chash = content_hash(&data);
            let pid = point_id(&cli.project, &rel_path, &sym.symbol_name, sym.start_line);

            // Skip if content unchanged
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
                serde_json::Value::String(cli.project.clone()),
            );
            payload.insert(
                "file_path".into(),
                serde_json::Value::String(rel_path.clone()),
            );
            payload.insert(
                "tech_stack".into(),
                serde_json::Value::String(tech_stack.to_string()),
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
                vector: vec![0.0; settings.embed_dims],
                payload: serde_json::Value::Object(payload),
            };

            batch.push((point, data));

            if batch.len() >= BATCH_SIZE {
                match flush_batch(&embedder, &store, table, &mut batch).await {
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

    // Flush remaining batch
    match flush_batch(&embedder, &store, table, &mut batch).await {
        Ok(n) => total_upserted += n,
        Err(e) => {
            warn!(error = %e, "Final batch embedding failed");
            total_errors += batch.len();
            batch.clear();
        }
    }

    // Delete points for removed files
    let removed_files: Vec<String> = indexed_file_paths
        .difference(&current_file_paths)
        .cloned()
        .collect();
    let mut total_deleted = 0usize;
    for fp in &removed_files {
        let filter = format!(
            "JSON_EXTRACT(payload, '$.project') = '{}' AND JSON_EXTRACT(payload, '$.file_path') = '{}'",
            cli.project.replace('\'', "''"),
            fp.replace('\'', "''")
        );
        match store.delete_by_filter(table, &filter).await {
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

    Ok(())
}

/// Embed and upsert a batch of points.
async fn flush_batch(
    embedder: &FastEmbedProvider,
    store: &LanceDbStore,
    table: &str,
    batch: &mut Vec<(VectorPoint, String)>,
) -> MindojoResult<usize> {
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
