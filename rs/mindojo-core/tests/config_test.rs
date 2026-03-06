//! Tests for config loading from defaults, env vars, and .env files.

use std::env;
use std::io::Write;

use mindojo_core::config::Settings;

// -- Test 1: default config --------------------------------------------------

#[test]
fn test_default_config() {
    let settings = Settings::default();

    assert_eq!(settings.ollama_url, "http://localhost:11434");
    assert!(settings.ollama_api_key.is_empty());
    assert_eq!(settings.lancedb_path, "~/.local/share/mindojo/lancedb");
    assert_eq!(settings.default_user_id, "global");
    assert!(settings.tech_stack.is_empty());
    assert!(settings.distill_memories);
    assert_eq!(settings.log_file, "~/.claude/logs/mindojo-mcp.log");
    assert_eq!(settings.llm_model, "qwen3.5:4b");
    assert_eq!(settings.embed_model, "qwen3-embedding:4b");
    assert_eq!(settings.embed_dims, 2560);
}

// -- Test 2: env var overrides -----------------------------------------------

#[test]
fn test_env_override() {
    // Save originals so we can restore after the test.
    let original_url = env::var("OLLAMA_URL").ok();
    let original_user = env::var("DEFAULT_USER_ID").ok();
    let original_dims = env::var("EMBED_DIMS").ok();
    let original_distill = env::var("DISTILL_MEMORIES").ok();

    // SAFETY: env::set_var is unsafe in edition 2024 because modifying the
    // environment is inherently racy when other threads read it. This test
    // is acceptable because cargo test runs each test function serially
    // within a single test binary when using --test-threads=1, and even
    // with parallel tests the worst case is a flaky assertion, not UB.
    unsafe {
        env::set_var("OLLAMA_URL", "http://custom-host:11434");
        env::set_var("DEFAULT_USER_ID", "test-user-42");
        env::set_var("EMBED_DIMS", "768");
        env::set_var("DISTILL_MEMORIES", "false");
    }

    let settings = Settings::load();

    assert_eq!(settings.ollama_url, "http://custom-host:11434");
    assert_eq!(settings.default_user_id, "test-user-42");
    assert_eq!(settings.embed_dims, 768);
    assert!(!settings.distill_memories);

    // Restore original env.
    unsafe {
        match original_url {
            Some(v) => env::set_var("OLLAMA_URL", v),
            None => env::remove_var("OLLAMA_URL"),
        }
        match original_user {
            Some(v) => env::set_var("DEFAULT_USER_ID", v),
            None => env::remove_var("DEFAULT_USER_ID"),
        }
        match original_dims {
            Some(v) => env::set_var("EMBED_DIMS", v),
            None => env::remove_var("EMBED_DIMS"),
        }
        match original_distill {
            Some(v) => env::set_var("DISTILL_MEMORIES", v),
            None => env::remove_var("DISTILL_MEMORIES"),
        }
    }
}

// -- Test 3: .env file loading -----------------------------------------------

#[test]
fn test_env_file() {
    // Create a temporary .env file and verify dotenvy can parse it.
    // We test the file format rather than Settings::load() directly,
    // because load() looks for .env in specific paths (config dir, CWD)
    // and we don't want to pollute the real filesystem.

    let tmp = tempfile::NamedTempFile::new().expect("create temp file");
    writeln!(
        tmp.as_file(),
        "OLLAMA_URL=http://from-dotenv:11434\n\
         OLLAMA_API_KEY=secret-from-file\n\
         DEFAULT_USER_ID=dotenv-user\n\
         EMBED_DIMS=1024\n\
         DISTILL_MEMORIES=false\n\
         TECH_STACK=rust"
    )
    .expect("write .env content");

    // Use dotenvy to load from the temp file path (not overriding existing env).
    let path = tmp.path();
    let vars: Vec<(String, String)> = dotenvy::from_path_iter(path)
        .expect("parse .env file")
        .filter_map(|r| r.ok())
        .collect();

    // Verify all expected keys are present.
    let find = |key: &str| -> String {
        vars.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("key {key} not found in .env"))
    };

    assert_eq!(find("OLLAMA_URL"), "http://from-dotenv:11434");
    assert_eq!(find("OLLAMA_API_KEY"), "secret-from-file");
    assert_eq!(find("DEFAULT_USER_ID"), "dotenv-user");
    assert_eq!(find("EMBED_DIMS"), "1024");
    assert_eq!(find("DISTILL_MEMORIES"), "false");
    assert_eq!(find("TECH_STACK"), "rust");
}
