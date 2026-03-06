/// Prompt for extracting individual facts from user-provided content.
pub const FACT_EXTRACTION_PROMPT: &str = include_str!("prompts/fact-extraction.md");

/// Prompt for extracting reusable technical lessons from hook conversations.
pub const FACT_EXTRACTION_HOOK_PROMPT: &str = include_str!("prompts/fact-extraction-hook.md");

/// Prompt for deduplicating new facts against existing memories.
pub const MEMORY_UPDATE_PROMPT: &str = include_str!("prompts/memory-update.md");

/// Prompt for extracting metadata from technical standards documents.
pub const METADATA_EXTRACTION_PROMPT: &str = include_str!("prompts/metadata-extraction.md");

/// Apply template substitution on a prompt string.
///
/// Replaces `$key` patterns with the corresponding values.
/// Supported placeholders: `$today`, `$existing_memories`, `$new_facts`, `$chunk_text`.
pub fn render_prompt(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
        // Replace $key patterns (word-boundary aware)
        let pattern = format!("${}", key);
        result = result.replace(&pattern, value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompts_are_non_empty() {
        assert!(!FACT_EXTRACTION_PROMPT.is_empty());
        assert!(!FACT_EXTRACTION_HOOK_PROMPT.is_empty());
        assert!(!MEMORY_UPDATE_PROMPT.is_empty());
        assert!(!METADATA_EXTRACTION_PROMPT.is_empty());
    }

    #[test]
    fn test_render_prompt() {
        let template = "Today is $today and user is $user_id";
        let result = render_prompt(template, &[("today", "2026-03-06"), ("user_id", "alice")]);
        assert_eq!(result, "Today is 2026-03-06 and user is alice");
    }

    #[test]
    fn test_fact_extraction_has_today_placeholder() {
        assert!(FACT_EXTRACTION_PROMPT.contains("$today"));
    }

    #[test]
    fn test_memory_update_has_placeholders() {
        assert!(MEMORY_UPDATE_PROMPT.contains("$existing_memories"));
        assert!(MEMORY_UPDATE_PROMPT.contains("$new_facts"));
    }
}
