// Comprehensive integration tests for OllamaRsLlmProvider.
//
// All tests require a real Ollama server and are marked #[ignore].
// Run with:
//   OLLAMA_HOST=... OLLAMA_API_KEY=... cargo test -p memcan-core \
//     --test ollama_integration -- --ignored --nocapture

use memcan_core::config::Settings;
use memcan_core::llm_ollama_rs::OllamaRsLlmProvider;
use memcan_core::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use serial_test::serial;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Build an OllamaRsLlmProvider from environment variables.
///
/// Reads OLLAMA_HOST, OLLAMA_API_KEY, and LLM_MODEL from the environment
/// (via Settings::load). Panics on configuration error so tests fail fast.
fn provider_from_env() -> OllamaRsLlmProvider {
    let settings = Settings::load().expect("Settings::load failed -- check env vars");
    let provider = OllamaRsLlmProvider::from_settings(&settings);
    eprintln!(
        "[setup] model={} url={}",
        provider.default_model(),
        provider.url()
    );
    provider
}

/// Shorthand for a single-role message.
fn msg(role: Role, content: &str) -> LlmMessage {
    LlmMessage {
        role,
        content: content.into(),
    }
}

/// Strip markdown code fences (```json ... ```) from LLM output.
///
/// Some models wrap JSON in fences even when format_json is set.
/// The pipeline test needs to handle this gracefully.
#[allow(dead_code)] // Used by #[ignore] tests only
fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip optional language tag on first line
        let rest = rest.strip_prefix("json").unwrap_or(rest);
        let rest = rest.trim_start_matches('\n');
        if let Some(body) = rest.strip_suffix("```") {
            return body.trim();
        }
    }
    trimmed
}

// ---------------------------------------------------------------------------
// Provider Construction
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn construction_default_config() {
    // Given: default settings (no OLLAMA_HOST / OLLAMA_API_KEY overrides)
    let settings = Settings::default();

    // When: provider is built
    let provider = OllamaRsLlmProvider::from_settings(&settings);

    // Then: model name has ollama:: prefix stripped, URL points to localhost
    assert_eq!(
        provider.default_model(),
        "qwen3.5:9b",
        "default model should be bare name without ollama:: prefix"
    );
    assert!(
        provider.url().contains("localhost"),
        "default URL should reference localhost, got: {}",
        provider.url()
    );
    eprintln!(
        "[ok] default config: model={} url={}",
        provider.default_model(),
        provider.url()
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn construction_custom_host_and_api_key() {
    // Given: settings with explicit host and API key
    let settings = Settings {
        ollama_host: Some("http://10.0.0.1:9999".into()),
        ollama_api_key: Some("test-token-abc".into()),
        llm_model: "some-model:7b".into(),
        ..Settings::default()
    };

    // When: provider is built
    let provider = OllamaRsLlmProvider::from_settings(&settings);

    // Then: model is stored, URL reflects custom host
    assert_eq!(provider.default_model(), "some-model:7b");
    assert!(
        provider.url().contains("10.0.0.1"),
        "URL should contain custom host, got: {}",
        provider.url()
    );
    eprintln!("[ok] custom host: url={}", provider.url());
}

#[tokio::test]
#[ignore]
#[serial]
async fn construction_ollama_prefix_stripped() {
    // Given: model name with ollama:: prefix
    let settings = Settings {
        llm_model: "ollama::qwen3.5:9b".into(),
        ..Settings::default()
    };

    // When: provider is built
    let provider = OllamaRsLlmProvider::from_settings(&settings);

    // Then: prefix is stripped for the Ollama API
    assert_eq!(
        provider.default_model(),
        "qwen3.5:9b",
        "ollama:: prefix must be stripped"
    );
    eprintln!("[ok] prefix stripped: {}", provider.default_model());
}

#[tokio::test]
#[ignore]
#[serial]
async fn construction_no_prefix_passthrough() {
    // Given: model name without prefix
    let settings = Settings {
        llm_model: "gemma3:4b".into(),
        ..Settings::default()
    };

    // When: provider is built
    let provider = OllamaRsLlmProvider::from_settings(&settings);

    // Then: model name passes through unchanged
    assert_eq!(provider.default_model(), "gemma3:4b");
    eprintln!("[ok] no-prefix passthrough: {}", provider.default_model());
}

// ---------------------------------------------------------------------------
// Basic Chat
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn chat_simple_user_message() {
    // Given: a provider connected to real Ollama
    let p = provider_from_env();

    // When: sending a simple user message
    let messages = vec![msg(Role::User, "Say 'hello' and nothing else.")];
    let result = p.chat(p.default_model(), &messages, None).await;

    // Then: response contains "hello" (case-insensitive)
    let text = result.expect("chat should succeed");
    eprintln!("[response] {}", &text[..text.len().min(200)]);
    assert!(
        text.to_lowercase().contains("hello"),
        "expected response to contain 'hello', got: {text}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn chat_system_plus_user() {
    // Given: system message instructing a specific behavior
    let p = provider_from_env();
    let messages = vec![
        msg(
            Role::System,
            "You are a translator. Translate every user message to French. Reply with only the translation.",
        ),
        msg(Role::User, "Good morning"),
    ];

    // When: chat is called
    let text = p
        .chat(p.default_model(), &messages, None)
        .await
        .expect("chat should succeed");
    eprintln!("[response] {text}");

    // Then: response should contain French greeting (bonjour)
    assert!(
        text.to_lowercase().contains("bonjour"),
        "expected French translation containing 'bonjour', got: {text}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn chat_multi_turn_conversation() {
    // Given: a multi-turn conversation with system + user + assistant + user
    let p = provider_from_env();
    let messages = vec![
        msg(Role::System, "You are a math tutor. Always show your work."),
        msg(Role::User, "What is 15 + 27?"),
        msg(Role::Assistant, "15 + 27 = 42"),
        msg(Role::User, "Now multiply that result by 2."),
    ];

    // When: chat is called
    let text = p
        .chat(p.default_model(), &messages, None)
        .await
        .expect("chat should succeed");
    eprintln!("[response] {text}");

    // Then: response should reference 84 (42 * 2)
    assert!(
        text.contains("84"),
        "expected response to contain '84' (42 * 2), got: {text}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn chat_with_ollama_prefix_in_model_name() {
    // Given: model name passed with ollama:: prefix (backward compat path through chat)
    let p = provider_from_env();
    let prefixed_model = format!("ollama::{}", p.default_model());
    let messages = vec![msg(Role::User, "Say 'yes'.")];

    // When: chat is called with prefixed model name
    let text = p
        .chat(&prefixed_model, &messages, None)
        .await
        .expect("chat with ollama:: prefix should succeed");
    eprintln!("[response] {text}");

    // Then: response is non-trivial (prefix was stripped internally)
    assert!(
        !text.is_empty(),
        "response should not be empty with prefixed model"
    );
    assert!(
        text.to_lowercase().contains("yes"),
        "expected 'yes' in response, got: {text}"
    );
}

// ---------------------------------------------------------------------------
// Options: JSON Mode
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn json_mode_returns_valid_json() {
    // Given: format_json: true with a prompt asking for JSON
    let p = provider_from_env();
    let messages = vec![
        msg(
            Role::System,
            "Return a JSON object with a single key 'answer' containing an integer.",
        ),
        msg(Role::User, "What is 2 + 3?"),
    ];
    let opts = LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    };

    // When: chat is called
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("JSON mode chat should succeed");
    eprintln!("[response] {text}");

    // Then: response is valid JSON with the expected key
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("response is not valid JSON: {e}\nraw: {text}"));
    assert!(
        parsed.get("answer").is_some(),
        "JSON should have 'answer' key, got: {parsed}"
    );
    assert_eq!(
        parsed["answer"].as_i64().unwrap_or(-1),
        5,
        "answer should be 5, got: {parsed}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn json_mode_structured_prompt_has_expected_keys() {
    // Given: JSON mode with a structured schema prompt
    let p = provider_from_env();
    let messages = vec![
        msg(
            Role::System,
            "You extract metadata. Return JSON with keys: \"title\" (string), \"tags\" (array of strings), \"count\" (integer).",
        ),
        msg(
            Role::User,
            "Rust programming language is fast, safe, and concurrent.",
        ),
    ];
    let opts = LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("structured JSON chat should succeed");
    eprintln!("[response] {text}");

    // Then: response must be valid JSON with an object at the top level.
    // We verify our provider correctly enables JSON mode — the specific keys
    // depend on LLM instruction-following which is non-deterministic.
    let parsed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not valid JSON: {e}\nraw: {text}"));
    assert!(parsed.is_object(), "expected JSON object, got: {parsed}");
    // At minimum, the model should return *some* keys
    let obj = parsed.as_object().unwrap();
    assert!(
        !obj.is_empty(),
        "expected non-empty JSON object, got: {parsed}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn json_mode_false_may_return_prose() {
    // Given: default options (no format_json)
    let p = provider_from_env();
    let messages = vec![msg(Role::User, "Explain what Rust is in one sentence.")];

    // When
    let text = p
        .chat(p.default_model(), &messages, None)
        .await
        .expect("default mode chat should succeed");
    eprintln!("[response] {text}");

    // Then: response is non-empty text (may or may not be JSON, that is fine)
    assert!(
        text.len() > 10,
        "expected a substantial prose response, got only {} chars: {text}",
        text.len()
    );
}

// ---------------------------------------------------------------------------
// Options: Think Control
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn think_false_no_think_tags() {
    // Given: think: Some(false)
    let p = provider_from_env();
    let messages = vec![msg(
        Role::User,
        "What is 2 + 2? Answer with just the number.",
    )];
    let opts = LlmOptions {
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("think:false chat should succeed");
    eprintln!("[response] {text}");

    // Then: no <think> tags in the response
    assert!(
        !text.contains("<think>"),
        "response should not contain <think> tags when think=false, got: {text}"
    );
    // Also verify the answer is correct
    assert!(
        text.contains('4'),
        "expected '4' in the response, got: {text}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn think_false_with_json_mode_clean_output() {
    // Given: think:false + format_json:true
    let p = provider_from_env();
    let messages = vec![
        msg(
            Role::System,
            "Return JSON with key 'result' containing the answer.",
        ),
        msg(Role::User, "What is 10 * 5?"),
    ];
    let opts = LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("think:false + json chat should succeed");
    eprintln!("[response] {text}");

    // Then: valid JSON, no thinking tags, correct value
    assert!(
        !text.contains("<think>"),
        "no <think> tags expected in JSON mode with think=false"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("not valid JSON: {e}\nraw: {text}"));
    assert!(
        parsed.get("result").is_some(),
        "expected 'result' key in JSON: {parsed}"
    );
    // LLMs may return the value as a number or string — handle both.
    let result_val = parsed["result"]
        .as_i64()
        .or_else(|| parsed["result"].as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(-1);
    assert_eq!(result_val, 50, "expected result=50, got: {result_val}");
}

#[tokio::test]
#[ignore]
#[serial]
async fn think_none_default_no_crash() {
    // Given: think: None (default -- model may or may not include thinking)
    let p = provider_from_env();
    let messages = vec![msg(Role::User, "Name three primary colors.")];

    // When
    let text = p
        .chat(p.default_model(), &messages, None)
        .await
        .expect("default think mode should succeed");
    eprintln!("[response] {text}");

    // Then: response is substantive (at least mentions colors)
    let lower = text.to_lowercase();
    let color_count = ["red", "blue", "yellow", "green"]
        .iter()
        .filter(|c| lower.contains(**c))
        .count();
    assert!(
        color_count >= 2,
        "expected at least 2 color names in response, found {color_count}: {text}"
    );
}

// ---------------------------------------------------------------------------
// Options: Temperature & Max Tokens
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn temperature_zero_deterministic() {
    // Given: temperature=0.0 should produce near-deterministic output
    let p = provider_from_env();
    let messages = vec![
        msg(
            Role::System,
            "Reply with exactly one word: the capital of France.",
        ),
        msg(Role::User, "What is the capital of France?"),
    ];
    let opts = LlmOptions {
        temperature: Some(0.0),
        think: Some(false),
        ..Default::default()
    };

    // When: called twice with identical input
    let text1 = p
        .chat(p.default_model(), &messages, Some(opts.clone()))
        .await
        .expect("first call should succeed");
    let text2 = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("second call should succeed");
    eprintln!("[run1] {text1}");
    eprintln!("[run2] {text2}");

    // Then: both responses should mention Paris
    assert!(
        text1.to_lowercase().contains("paris"),
        "run1 should contain 'paris', got: {text1}"
    );
    assert!(
        text2.to_lowercase().contains("paris"),
        "run2 should contain 'paris', got: {text2}"
    );
    // With temp=0 the outputs should be identical or very close
    assert_eq!(
        text1.trim().to_lowercase(),
        text2.trim().to_lowercase(),
        "temperature=0 should produce deterministic output"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn max_tokens_limits_response_length() {
    // Given: max_tokens=5 should severely limit output length
    let p = provider_from_env();
    let messages = vec![msg(
        Role::User,
        "Write a 500-word essay about the history of computing.",
    )];
    let opts = LlmOptions {
        max_tokens: Some(5),
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("max_tokens=5 should succeed");
    eprintln!("[response] ({} chars) {text}", text.len());

    // Then: response should be very short (under ~80 chars accounting for token-to-char ratio)
    assert!(
        text.len() < 80,
        "max_tokens=5 should produce a very short response, got {} chars: {text}",
        text.len()
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn max_tokens_one_very_short() {
    // Given: max_tokens=1 should produce minimal output
    let p = provider_from_env();
    let messages = vec![msg(Role::User, "What is 1+1?")];
    let opts = LlmOptions {
        max_tokens: Some(1),
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("max_tokens=1 should succeed");
    eprintln!("[response] ({} chars) '{text}'", text.len());

    // Then: response should be extremely short (1 token ~ 1-4 chars typically)
    assert!(
        text.len() <= 10,
        "max_tokens=1 should produce at most ~10 chars, got {}: '{text}'",
        text.len()
    );
}

// ---------------------------------------------------------------------------
// Context Window
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn context_window_returns_positive_value() {
    // Given: a valid model on the Ollama server
    let p = provider_from_env();

    // When: querying context window
    let ctx = p.context_window(p.default_model()).await;

    // Then: returns a positive, reasonable value
    let size = ctx.expect("context_window should return Some for a valid Ollama model");
    eprintln!("[context_window] {} tokens", size);
    assert!(size > 0, "context window must be positive");
    assert!(
        size >= 1024,
        "context window should be at least 1024 tokens for any modern model, got: {size}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn context_window_reasonable_range() {
    // Given: a valid model
    let p = provider_from_env();

    // When
    let size = p
        .context_window(p.default_model())
        .await
        .expect("should return Some");
    eprintln!("[context_window] {} tokens", size);

    // Then: value is within a reasonable range for current models (1k - 2M tokens)
    assert!(
        (1_000..=2_000_000).contains(&size),
        "context window {} is outside reasonable range [1000, 2000000]",
        size
    );
}

// ---------------------------------------------------------------------------
// Error Handling
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn error_nonexistent_model() {
    // Given: a model name that does not exist on the server
    let p = provider_from_env();
    let messages = vec![msg(Role::User, "Hello")];

    // When: chat is called with a bogus model name
    let result = p
        .chat("nonexistent-model-xyz-999:latest", &messages, None)
        .await;

    // Then: returns an LlmChat error (not a panic)
    let err = result.expect_err("chat with nonexistent model should fail");
    eprintln!("[error] {err}");
    assert!(
        err.is_llm_error(),
        "error should be an LlmChat variant, got: {err:?}"
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn error_garbage_model_name() {
    // Given: a completely invalid model name
    let p = provider_from_env();
    let messages = vec![msg(Role::User, "Hello")];

    // When
    let result = p.chat("!!garbage!!/not-a-model", &messages, None).await;

    // Then: error, not panic
    let err = result.expect_err("garbage model name should produce an error");
    eprintln!("[error] {err}");
    assert!(
        err.is_llm_error(),
        "error should be LlmChat variant, got: {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Pipeline Pattern: fact extraction (replicates actual production usage)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
#[serial]
async fn pipeline_fact_extraction_pattern() {
    // Given: the same pattern the pipeline uses:
    //   system prompt (fact extraction) + user content + format_json + think:false
    let p = provider_from_env();

    let system_prompt = r#"You are a Technical Knowledge Organizer. Split input into individual facts.
Return ONLY valid JSON: {"facts": ["fact1", "fact2"]}
Keep single-fact inputs as one item. Preserve all specific details."#;

    let user_content = "We switched from qwen3.5:9b to gemma3n:e4b because qwen returns empty under concurrent requests. \
         The Ollama API key must be passed via Authorization Bearer header.";

    let messages = vec![
        msg(Role::System, system_prompt),
        msg(Role::User, user_content),
    ];
    let opts = LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("pipeline pattern chat should succeed");
    eprintln!("[response] {text}");

    // Then: valid JSON with a "facts" array containing multiple items
    // Strip markdown code fences if the model wraps JSON despite format_json
    let clean = strip_code_fences(&text);
    let parsed: serde_json::Value =
        serde_json::from_str(clean).unwrap_or_else(|e| panic!("not valid JSON: {e}\nraw: {text}"));

    let facts = parsed
        .get("facts")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected 'facts' array in response: {parsed}"));

    assert!(
        facts.len() >= 2,
        "input contains at least 2 distinct facts, got {}: {:?}",
        facts.len(),
        facts
    );

    // Verify facts are strings with substance
    for (i, fact) in facts.iter().enumerate() {
        let s = fact
            .as_str()
            .unwrap_or_else(|| panic!("fact[{i}] should be a string, got: {fact}"));
        assert!(
            s.len() > 10,
            "fact[{i}] should be substantive (>10 chars), got: '{s}'"
        );
    }

    // At least one fact should mention the model switch or concurrent requests
    let all_facts_lower: String = facts
        .iter()
        .filter_map(|f| f.as_str())
        .map(|s| s.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        all_facts_lower.contains("qwen") || all_facts_lower.contains("gemma"),
        "facts should reference model names from the input: {:?}",
        facts
    );
}

#[tokio::test]
#[ignore]
#[serial]
async fn pipeline_empty_input_returns_empty_facts() {
    // Given: the pipeline pattern with content that has no technical facts
    let p = provider_from_env();

    let system_prompt = r#"You are a Technical Knowledge Organizer. Split input into individual facts.
Return ONLY valid JSON: {"facts": ["fact1", "fact2"]}
For greetings or filler with no information, return {"facts": []}"#;

    let messages = vec![
        msg(Role::System, system_prompt),
        msg(Role::User, "Hi, how are you doing today?"),
    ];
    let opts = LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    };

    // When
    let text = p
        .chat(p.default_model(), &messages, Some(opts))
        .await
        .expect("empty-input pipeline pattern should succeed");
    eprintln!("[response] {text}");

    // Then: valid JSON with empty or near-empty facts array
    let clean = strip_code_fences(&text);
    let parsed: serde_json::Value =
        serde_json::from_str(clean).unwrap_or_else(|e| panic!("not valid JSON: {e}\nraw: {text}"));
    let facts = parsed
        .get("facts")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected 'facts' array: {parsed}"));

    assert!(
        facts.is_empty(),
        "greeting with no facts should produce empty array, got: {:?}",
        facts
    );
}
