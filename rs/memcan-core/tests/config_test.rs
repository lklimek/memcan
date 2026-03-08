//! Tests for config loading from defaults, env vars, and .env files.

use std::env;
use std::io::Write;

use memcan_core::config::Settings;
use serial_test::serial;

// -- Test 1: default config --------------------------------------------------

#[test]
fn test_default_config() {
    let settings = Settings::default();

    assert_eq!(settings.lancedb_path, "~/.local/share/memcan/lancedb");
    assert_eq!(settings.default_user_id, "global");
    assert!(settings.tech_stack.is_empty());
    assert!(settings.distill_memories);
    assert_eq!(settings.log_file, "~/.claude/logs/memcan-mcp.log");
    assert_eq!(settings.llm_model, "ollama::qwen3.5:4b");
    assert_eq!(settings.embed_model, "MultilingualE5Large");
    assert_eq!(settings.embed_dims, 1024);
}

// -- Test 2: env var overrides -----------------------------------------------

#[test]
#[serial]
fn test_env_override() {
    // Save originals so we can restore after the test.
    let original_user = env::var("DEFAULT_USER_ID").ok();
    let original_distill = env::var("DISTILL_MEMORIES").ok();
    let original_llm = env::var("LLM_MODEL").ok();
    let original_embed = env::var("EMBED_MODEL").ok();

    // SAFETY: env::set_var is unsafe in edition 2024 because modifying the
    // environment is inherently racy when other threads read it. This is
    // acceptable here because cargo runs integration tests as separate
    // binaries, and within a binary the test runner defaults to serial
    // execution for #[test] functions in the same file.
    unsafe {
        env::set_var("DEFAULT_USER_ID", "test-user-42");
        env::set_var("EMBED_MODEL", "NomicEmbedTextV15");
        env::set_var("DISTILL_MEMORIES", "false");
        env::set_var("LLM_MODEL", "openai::gpt-4o");
    }

    let settings = Settings::load().expect("load should succeed");

    assert_eq!(settings.default_user_id, "test-user-42");
    assert_eq!(settings.embed_model, "NomicEmbedTextV15");
    assert_eq!(
        settings.embed_dims, 768,
        "dims should be derived from NomicEmbedTextV15"
    );
    assert!(!settings.distill_memories);
    assert_eq!(settings.llm_model, "openai::gpt-4o");

    // Restore original env.
    unsafe {
        match original_user {
            Some(v) => env::set_var("DEFAULT_USER_ID", v),
            None => env::remove_var("DEFAULT_USER_ID"),
        }
        match original_distill {
            Some(v) => env::set_var("DISTILL_MEMORIES", v),
            None => env::remove_var("DISTILL_MEMORIES"),
        }
        match original_llm {
            Some(v) => env::set_var("LLM_MODEL", v),
            None => env::remove_var("LLM_MODEL"),
        }
        match original_embed {
            Some(v) => env::set_var("EMBED_MODEL", v),
            None => env::remove_var("EMBED_MODEL"),
        }
    }
}

// -- Test 3: .env file loading -----------------------------------------------

#[test]
#[serial]
fn test_env_file() {
    // Create a temporary .env file and verify dotenvy can parse it.
    // We test the file format rather than Settings::load() directly,
    // because load() looks for .env in specific paths (config dir, CWD)
    // and we don't want to pollute the real filesystem.

    let tmp = tempfile::NamedTempFile::new().expect("create temp file");
    writeln!(
        tmp.as_file(),
        "DEFAULT_USER_ID=dotenv-user\n\
         DISTILL_MEMORIES=false\n\
         TECH_STACK=rust\n\
         LLM_MODEL=ollama::mistral:7b\n\
         EMBED_MODEL=BGESmallENV15"
    )
    .expect("write .env content");

    let path = tmp.path();
    let vars: Vec<(String, String)> = dotenvy::from_path_iter(path)
        .expect("parse .env file")
        .filter_map(|r| r.ok())
        .collect();

    let find = |key: &str| -> String {
        vars.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| panic!("key {key} not found in .env"))
    };

    assert_eq!(find("DEFAULT_USER_ID"), "dotenv-user");
    assert_eq!(find("DISTILL_MEMORIES"), "false");
    assert_eq!(find("TECH_STACK"), "rust");
    assert_eq!(find("LLM_MODEL"), "ollama::mistral:7b");
    assert_eq!(find("EMBED_MODEL"), "BGESmallENV15");
}
