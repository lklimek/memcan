//! Local file walking for index-code command.
//!
//! No dependency on memcan-core — just std filesystem operations.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

// Keep in sync with SKIP_DIRS in rs/memcan-core/src/indexing/code.rs.
const SKIP_DIRS: &[&str] = &[
    ".claude",
    ".git",
    ".next",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "vendor",
];

const ALLOWED_EXTENSIONS: &[&str] = &["rs", "py", "go", "ts", "tsx"];

/// The closed set of recognized `--tech-stack` values (canonical lowercase).
pub const SUPPORTED_TECH_STACKS: &[&str] = &["rust", "python", "go", "typescript"];

/// Canonicalize a user-supplied tech-stack name to its lowercase form.
///
/// Matching is case-insensitive (`Rust` -> `rust`). Returns `None` for an
/// unrecognized stack so callers can reject it loudly. The canonical name is
/// what gets stored on each record, so server-side exact-match search finds it.
pub fn canonical_tech_stack(stack: &str) -> Option<&'static str> {
    SUPPORTED_TECH_STACKS
        .iter()
        .copied()
        .find(|s| *s == stack.to_ascii_lowercase())
}

/// Maps an explicit tech stack to the file extensions it permits.
///
/// Matching is case-insensitive (`Rust` == `rust`). Returns `None` for an
/// unrecognized stack so the caller can reject it loudly rather than silently
/// indexing every language.
///
/// Keep the stack->extension mapping in sync with `lang_extensions()` in
/// rs/memcan-core/src/indexing/code.rs.
fn extensions_for_stack(stack: &str) -> Option<&'static [&'static str]> {
    match canonical_tech_stack(stack)? {
        "rust" => Some(&["rs"]),
        "python" => Some(&["py"]),
        "go" => Some(&["go"]),
        "typescript" => Some(&["ts", "tsx"]),
        _ => None,
    }
}

pub struct WalkOptions<'a> {
    pub max_file_size: u64,
    /// When `Some`, restrict walked files to the extensions of this tech stack.
    /// When `None`, walk all supported extensions.
    pub tech_stack: Option<&'a str>,
}

#[derive(Debug)]
pub struct WalkedFile {
    pub relative_path: String,
    pub content: String,
}

pub fn walk_directory(root: &Path, opts: &WalkOptions) -> Result<Vec<WalkedFile>, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve root directory: {e}"))?;
    let allowed: &[&str] = match opts.tech_stack {
        Some(stack) => extensions_for_stack(stack).ok_or_else(|| {
            format!("unknown --tech-stack '{stack}'; supported: rust, python, go, typescript")
        })?,
        None => ALLOWED_EXTENSIONS,
    };
    let mut files = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();

            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };

            if ft.is_symlink() {
                continue;
            }

            if ft.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !SKIP_DIRS.contains(&name_str.as_ref()) {
                    stack.push(path);
                }
                continue;
            }

            if !ft.is_file() {
                continue;
            }

            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_ascii_lowercase(),
                None => continue,
            };
            if !allowed.contains(&ext.as_str()) {
                continue;
            }

            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > opts.max_file_size {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            files.push(WalkedFile {
                relative_path: relative,
                content,
            });
        }
    }

    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(files)
}

pub fn detect_tech_stack(files: &[WalkedFile]) -> Result<String, String> {
    let mut exts: HashSet<String> = HashSet::new();
    for f in files {
        if let Some(ext) = Path::new(&f.relative_path)
            .extension()
            .and_then(|e| e.to_str())
        {
            exts.insert(ext.to_ascii_lowercase());
        }
    }

    let has_rs = exts.contains("rs");
    let has_py = exts.contains("py");
    let has_go = exts.contains("go");
    let has_ts = exts.contains("ts") || exts.contains("tsx");

    let count = [has_rs, has_py, has_go, has_ts]
        .iter()
        .filter(|&&b| b)
        .count();

    if count == 0 {
        return Err("no supported files found".to_string());
    }
    if count > 1 {
        return Err("mixed languages detected, please specify --tech-stack explicitly".to_string());
    }

    if has_rs {
        Ok("rust".to_string())
    } else if has_py {
        Ok("python".to_string())
    } else if has_go {
        Ok("go".to_string())
    } else {
        Ok("typescript".to_string())
    }
}

pub fn chunk_into_batches<T>(items: Vec<T>, batch_size: usize) -> Vec<Vec<T>> {
    let batch_size = batch_size.max(1);
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(batch_size);
    for item in items {
        current.push(item);
        if current.len() >= batch_size {
            batches.push(std::mem::take(&mut current));
            current = Vec::with_capacity(batch_size);
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Create valid source files
        fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("lib.py"), "print('hello')").unwrap();

        // Create a subdirectory with a file
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src").join("util.rs"), "pub fn util() {}").unwrap();

        // Create skip directories
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("config"), "git config").unwrap();

        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules").join("pkg.ts"), "export {}").unwrap();

        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target").join("debug.rs"), "// build output").unwrap();

        fs::create_dir_all(root.join(".claude").join("worktrees").join("agent-abc")).unwrap();
        fs::write(
            root.join(".claude")
                .join("worktrees")
                .join("agent-abc")
                .join("stray.rs"),
            "fn stray() {}",
        )
        .unwrap();

        // Create unsupported extension
        fs::write(root.join("readme.md"), "# Readme").unwrap();
        fs::write(root.join("data.json"), "{}").unwrap();

        dir
    }

    #[test]
    fn walk_finds_supported_files() {
        let dir = setup_test_dir();
        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: None,
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        assert!(paths.contains(&"main.rs"));
        assert!(paths.contains(&"lib.py"));
        assert!(paths.contains(&"src/util.rs"));
    }

    #[test]
    fn walk_skips_excluded_dirs() {
        let dir = setup_test_dir();
        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: None,
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        for p in &paths {
            assert!(!p.starts_with(".git/"), "should skip .git: {p}");
            assert!(
                !p.starts_with("node_modules/"),
                "should skip node_modules: {p}"
            );
            assert!(!p.starts_with("target/"), "should skip target: {p}");
            assert!(!p.starts_with(".claude/"), "should skip .claude: {p}");
        }
    }

    #[test]
    fn walk_skips_unsupported_extensions() {
        let dir = setup_test_dir();
        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: None,
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        assert!(!paths.contains(&"readme.md"));
        assert!(!paths.contains(&"data.json"));
    }

    #[test]
    fn walk_skips_large_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("big.rs"), "x".repeat(200)).unwrap();
        fs::write(dir.path().join("small.rs"), "fn f() {}").unwrap();

        let opts = WalkOptions {
            max_file_size: 100,
            tech_stack: None,
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        assert!(!paths.contains(&"big.rs"));
        assert!(paths.contains(&"small.rs"));
    }

    #[test]
    fn walk_restricts_extensions_to_explicit_tech_stack() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();
        fs::write(dir.path().join("b.ts"), "export {}").unwrap();

        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: Some("rust"),
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        assert!(paths.contains(&"a.rs"));
        assert!(!paths.contains(&"b.ts"), "should skip .ts when stack=rust");
    }

    #[test]
    fn walk_rejects_unknown_explicit_tech_stack() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.rs"), "fn a() {}").unwrap();

        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: Some("rsut"),
        };
        let err = walk_directory(dir.path(), &opts).unwrap_err();
        assert!(err.contains("unknown --tech-stack 'rsut'"), "got: {err}");
    }

    #[test]
    fn walk_tech_stack_is_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.RS"), "fn a() {}").unwrap();
        fs::write(dir.path().join("b.ts"), "export {}").unwrap();

        let opts = WalkOptions {
            max_file_size: 1_048_576,
            tech_stack: Some("Rust"),
        };
        let files = walk_directory(dir.path(), &opts).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.relative_path.as_str()).collect();

        assert!(paths.contains(&"a.RS"), "uppercase .RS should match rust");
        assert!(!paths.contains(&"b.ts"));
    }

    #[test]
    fn detect_tech_stack_is_case_insensitive() {
        let files = vec![WalkedFile {
            relative_path: "src/Main.RS".into(),
            content: String::new(),
        }];
        assert_eq!(detect_tech_stack(&files).unwrap(), "rust");
    }

    #[test]
    fn detect_single_language_rust() {
        let files = vec![
            WalkedFile {
                relative_path: "src/main.rs".into(),
                content: String::new(),
            },
            WalkedFile {
                relative_path: "src/lib.rs".into(),
                content: String::new(),
            },
        ];
        assert_eq!(detect_tech_stack(&files).unwrap(), "rust");
    }

    #[test]
    fn detect_single_language_python() {
        let files = vec![WalkedFile {
            relative_path: "app.py".into(),
            content: String::new(),
        }];
        assert_eq!(detect_tech_stack(&files).unwrap(), "python");
    }

    #[test]
    fn detect_single_language_typescript() {
        let files = vec![
            WalkedFile {
                relative_path: "index.ts".into(),
                content: String::new(),
            },
            WalkedFile {
                relative_path: "App.tsx".into(),
                content: String::new(),
            },
        ];
        assert_eq!(detect_tech_stack(&files).unwrap(), "typescript");
    }

    #[test]
    fn detect_mixed_languages_errors() {
        let files = vec![
            WalkedFile {
                relative_path: "main.rs".into(),
                content: String::new(),
            },
            WalkedFile {
                relative_path: "script.py".into(),
                content: String::new(),
            },
        ];
        let err = detect_tech_stack(&files).unwrap_err();
        assert!(err.contains("mixed languages"));
    }

    #[test]
    fn detect_no_files_errors() {
        let files: Vec<WalkedFile> = vec![];
        let err = detect_tech_stack(&files).unwrap_err();
        assert!(err.contains("no supported files"));
    }

    #[test]
    fn chunk_batches_exact() {
        let items: Vec<i32> = (1..=6).collect();
        let batches = chunk_into_batches(items, 3);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0], vec![1, 2, 3]);
        assert_eq!(batches[1], vec![4, 5, 6]);
    }

    #[test]
    fn chunk_batches_remainder() {
        let items: Vec<i32> = (1..=7).collect();
        let batches = chunk_into_batches(items, 3);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[2], vec![7]);
    }

    #[test]
    fn chunk_batches_empty() {
        let items: Vec<i32> = vec![];
        let batches = chunk_into_batches(items, 3);
        assert!(batches.is_empty());
    }

    #[test]
    fn chunk_batches_single_item() {
        let items = vec![42];
        let batches = chunk_into_batches(items, 5);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], vec![42]);
    }
}
