use handlebars::Handlebars;
use tracing::warn;

/// Prompt for extracting individual facts from user-provided content.
pub const FACT_EXTRACTION_PROMPT: &str = include_str!("prompts/fact-extraction.md");

/// Prompt for extracting reusable technical lessons from hook conversations.
pub const FACT_EXTRACTION_HOOK_PROMPT: &str = include_str!("prompts/fact-extraction-hook.md");

/// Prompt for deduplicating new facts against existing memories.
pub const MEMORY_UPDATE_PROMPT: &str = include_str!("prompts/memory-update.md");

/// Prompt for extracting metadata from technical standards documents.
pub const METADATA_EXTRACTION_PROMPT: &str = include_str!("prompts/metadata-extraction.md");

/// Render a prompt template with variable substitution.
///
/// Uses handlebars syntax (`{{key}}`). Falls back to legacy `$key` replacement
/// so existing `.md` templates with `$`-style placeholders continue to work.
pub fn render_prompt(template: &str, vars: &[(&str, &str)]) -> String {
    // Convert $key placeholders to {{key}} for handlebars compatibility.
    let hbs_template = convert_legacy_placeholders(template);

    let mut hbs = Handlebars::new();
    hbs.set_strict_mode(false);

    let mut data = serde_json::Map::new();
    for (key, value) in vars {
        data.insert(
            (*key).to_string(),
            serde_json::Value::String((*value).to_string()),
        );
    }

    match hbs.render_template(&hbs_template, &data) {
        Ok(rendered) => rendered,
        Err(e) => {
            warn!(error = %e, "handlebars render failed, falling back to naive replace");
            naive_replace(template, vars)
        }
    }
}

/// Convert `$key` patterns to `{{key}}` for handlebars.
fn convert_legacy_placeholders(template: &str) -> String {
    let re = regex::Regex::new(r"\$([a-zA-Z_][a-zA-Z0-9_]*)").unwrap();
    re.replace_all(template, "{{$1}}").into_owned()
}

/// Fallback: simple string replacement (original behavior).
fn naive_replace(template: &str, vars: &[(&str, &str)]) -> String {
    let mut result = template.to_string();
    for (key, value) in vars {
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
    fn test_render_prompt_legacy_dollar() {
        let template = "Today is $today and user is $user_id";
        let result = render_prompt(template, &[("today", "2026-03-06"), ("user_id", "alice")]);
        assert_eq!(result, "Today is 2026-03-06 and user is alice");
    }

    #[test]
    fn test_render_prompt_handlebars_native() {
        let template = "Today is {{today}} and user is {{user_id}}";
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

    #[test]
    fn test_render_prompt_missing_var_no_panic() {
        let template = "Hello $name, today is $date";
        let result = render_prompt(template, &[("name", "Alice")]);
        assert!(result.contains("Alice"));
    }

    #[test]
    fn test_convert_legacy_placeholders() {
        let input = "Hello $name, your $project_id is ready";
        let output = convert_legacy_placeholders(input);
        assert_eq!(output, "Hello {{name}}, your {{project_id}} is ready");
    }
}
