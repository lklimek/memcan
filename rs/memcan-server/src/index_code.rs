//! Index source code into vector store (moved from `memcan-index-code`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use memcan_core::error::{MemcanError, Result as MemcanResult, ResultExt};
use memcan_core::init::MemcanContext;
use memcan_core::pipeline::CODE_TABLE;
use memcan_core::traits::{EmbeddingProvider, VectorPoint, VectorStore};

use crate::IndexCodeArgs;

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
) -> MemcanResult<usize> {
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

pub async fn run(args: &IndexCodeArgs) -> MemcanResult<()> {
    let ctx = MemcanContext::init().await?;
    let table = CODE_TABLE;

    if args.drop {
        ctx.store
            .ensure_table(table, ctx.settings.embed_dims)
            .await?;
        let filter = format!("project = '{}'", args.project.replace('\'', "''"));
        let deleted = ctx.store.delete_by_filter(table, &filter).await?;
        info!(deleted, project = %args.project, "Dropped indexed data");
        return Ok(());
    }

    let tech_stack = args.tech_stack.as_deref().ok_or_else(|| {
        MemcanError::Other("--tech-stack is required unless --drop is specified".into())
    })?;

    let project_dir = args.project_dir.canonicalize().with_context(|| {
        format!(
            "Project directory does not exist: {}",
            args.project_dir.display()
        )
    })?;

    if !project_dir.is_dir() {
        return Err(MemcanError::Other(format!(
            "Not a directory: {}",
            project_dir.display()
        )));
    }

    ctx.store
        .ensure_table(table, ctx.settings.embed_dims)
        .await?;

    let git_hash = git_short_hash(&project_dir);
    let now = Utc::now().to_rfc3339();

    let project_filter = format!("project = '{}'", args.project.replace('\'', "''"));
    let existing_records = ctx
        .store
        .scroll(table, Some(&project_filter), 100_000, 0)
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
        if file_size > args.max_file_size {
            warn!(path = %rel_path, size = file_size, limit = args.max_file_size, "Skipping oversized file");
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
            let ctx_line = context_line(&rel_path, effective_lang, tech_stack);
            let data = format!("{}\n{}", ctx_line, sym.text);
            let chash = content_hash(&data);
            let pid = point_id(&args.project, &rel_path, &sym.symbol_name, sym.start_line);

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
                serde_json::Value::String(args.project.clone()),
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
                vector: vec![0.0; ctx.settings.embed_dims],
                payload: serde_json::Value::Object(payload),
            };

            batch.push((point, data));

            if batch.len() >= BATCH_SIZE {
                match flush_batch(&ctx.embedder, &ctx.store, table, &mut batch).await {
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

    match flush_batch(&ctx.embedder, &ctx.store, table, &mut batch).await {
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
            args.project.replace('\'', "''"),
            fp.replace('\'', "''")
        );
        match ctx.store.delete_by_filter(table, &filter).await {
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
