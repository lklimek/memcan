// Integration tests validating the OUTPUT QUALITY of fact-extraction.md.
//
// These tests require a live Ollama server and are marked #[ignore].
// They assert *output properties* — self-containment, parallel-symbol collapse,
// timeless phrasing — which the `test-classification` keep/reject tool cannot check.
//
// Run with:
//   OLLAMA_HOST=... OLLAMA_API_KEY=... cargo test -p memcan-core \
//     --test fact_extraction_quality -- --ignored --nocapture

use memcan_core::config::Settings;
use memcan_core::llm_ollama_rs::OllamaRsLlmProvider;
use memcan_core::prompts::{FACT_EXTRACTION_PROMPT, render_prompt};
use memcan_core::traits::{LlmMessage, LlmOptions, LlmProvider, Role};
use serial_test::serial;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn provider_from_env() -> OllamaRsLlmProvider {
    let settings = Settings::load().expect("Settings::load failed — check env vars");
    OllamaRsLlmProvider::from_settings(&settings)
}

fn extraction_messages(input: &str) -> Vec<LlmMessage> {
    // Use the real production prompt so these tests validate the actual artifact.
    let rendered = render_prompt(FACT_EXTRACTION_PROMPT, &[("today", "2026-06-30")]);
    vec![
        LlmMessage {
            role: Role::System,
            content: rendered,
        },
        LlmMessage {
            role: Role::User,
            content: input.to_string(),
        },
    ]
}

fn extraction_opts() -> LlmOptions {
    LlmOptions {
        format_json: true,
        think: Some(false),
        ..Default::default()
    }
}

/// Parse `{"facts": [...]}` JSON; panics with a helpful message on failure.
fn parse_facts(raw: &str) -> Vec<String> {
    // Strip optional markdown code fences (some models wrap JSON despite format_json).
    let trimmed = raw.trim();
    let clean = if let Some(rest) = trimmed.strip_prefix("```") {
        let rest = rest
            .strip_prefix("json")
            .unwrap_or(rest)
            .trim_start_matches('\n');
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    };

    let parsed: serde_json::Value =
        serde_json::from_str(clean).unwrap_or_else(|e| panic!("not valid JSON: {e}\nraw: {raw}"));

    parsed
        .get("facts")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("expected 'facts' array in: {parsed}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("fact should be string, got: {v}"))
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rule: Self-containment — no dangling anaphora in extracted facts
// ---------------------------------------------------------------------------

/// An input whose two sentences describe a cause and its fix using "this issue"
/// as an anaphoric reference.  The Self-containment rule requires the model to
/// inline the referent rather than emitting a fragment that says "this issue".
#[tokio::test]
#[ignore]
#[serial]
async fn fact_extraction_self_contained_no_anaphora() {
    let p = provider_from_env();
    let input = "The store's manifest file can be partially written \
        if the process is killed during a flush operation. \
        This issue causes all subsequent table-open calls to fail with a parse error.";

    let messages = extraction_messages(input);
    let raw = p
        .chat(p.default_model(), &messages, Some(extraction_opts()))
        .await
        .expect("chat should succeed");
    eprintln!("[response] {raw}");

    let facts = parse_facts(&raw);
    assert!(
        !facts.is_empty(),
        "technical input must produce at least one fact"
    );

    let all_lower = facts.join(" ").to_lowercase();

    // The model must NOT carry the dangling "this issue" into any fact.
    // Acceptable: inline references like "partially-written manifest" are fine.
    assert!(
        !all_lower.contains("this issue"),
        "facts must not contain dangling 'this issue'; got: {facts:?}"
    );
    assert!(
        !all_lower.contains("this bug"),
        "facts must not contain dangling 'this bug'; got: {facts:?}"
    );
    assert!(
        !all_lower.contains("this problem"),
        "facts must not contain dangling 'this problem'; got: {facts:?}"
    );

    // Positive check: the combined facts should still convey the core information.
    assert!(
        all_lower.contains("manifest")
            || all_lower.contains("flush")
            || all_lower.contains("parse"),
        "facts must preserve the substance of the input; got: {facts:?}"
    );

    eprintln!(
        "[ok] extracted {count} fact(s), none contain dangling anaphora",
        count = facts.len()
    );
}

// ---------------------------------------------------------------------------
// Rule: Collapse near-duplicate parallel facts
// ---------------------------------------------------------------------------

/// Two sibling functions with IDENTICAL behaviour described in near-duplicate
/// sentences.  The collapse rule requires ONE merged fact, not two.
#[tokio::test]
#[ignore]
#[serial]
async fn fact_extraction_collapses_parallel_symbols() {
    let p = provider_from_env();
    // Explicitly redundant: the two sentences make the same claim about two symbols.
    let input = "batch_embed() acquires the embedding semaphore before processing any input. \
        stream_embed() also acquires the embedding semaphore before processing any input.";

    let messages = extraction_messages(input);
    let raw = p
        .chat(p.default_model(), &messages, Some(extraction_opts()))
        .await
        .expect("chat should succeed");
    eprintln!("[response] {raw}");

    let facts = parse_facts(&raw);

    // The two sentences express ONE claim (same semaphore, same timing, same reason).
    // The model should collapse them into a single merged fact.
    assert_eq!(
        facts.len(),
        1,
        "near-identical parallel claims must collapse to 1 fact; got {}: {facts:?}",
        facts.len()
    );

    let fact_lower = facts[0].to_lowercase();
    // The merged fact must mention both symbols and the semaphore.
    assert!(
        fact_lower.contains("batch_embed") && fact_lower.contains("stream_embed"),
        "merged fact must name both symbols; got: {:?}",
        facts[0]
    );
    assert!(
        fact_lower.contains("semaphore"),
        "merged fact must preserve the semaphore detail; got: {:?}",
        facts[0]
    );

    eprintln!(
        "[ok] parallel symbols correctly collapsed to 1 fact: {:?}",
        facts[0]
    );
}

// ---------------------------------------------------------------------------
// Rule: Timeless present tense — no change-relative words in output
// ---------------------------------------------------------------------------

/// Input uses "now" and "previously" to describe a change.  The timeless rule
/// requires the model to state the durable end-state without those words.
#[tokio::test]
#[ignore]
#[serial]
async fn fact_extraction_timeless_tense() {
    let p = provider_from_env();
    let input = "The index builder now skips hidden files (names starting with a dot). \
        Previously it included all files in the target directory regardless of the leading dot.";

    let messages = extraction_messages(input);
    let raw = p
        .chat(p.default_model(), &messages, Some(extraction_opts()))
        .await
        .expect("chat should succeed");
    eprintln!("[response] {raw}");

    let facts = parse_facts(&raw);
    assert!(
        !facts.is_empty(),
        "technical input must produce at least one fact"
    );

    let all_lower = facts.join(" ").to_lowercase();

    // Change-relative words are banned by the timeless rule.
    let temporal_words = ["previously", " now ", "currently", "no longer", "as of"];
    for word in temporal_words {
        assert!(
            !all_lower.contains(word),
            "fact must not contain change-relative word {word:?}; facts: {facts:?}"
        );
    }

    // Positive check: the durable end-state must be preserved.
    assert!(
        all_lower.contains("hidden") || all_lower.contains("dot") || all_lower.contains("skip"),
        "facts must preserve the end-state (skip hidden files); got: {facts:?}"
    );

    eprintln!("[ok] no temporal words in facts: {facts:?}");
}
