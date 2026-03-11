//! Shared query helpers: user ID resolution, SQL sanitization, result formatting.

/// Escape single quotes for SQL equality comparisons.
pub fn sanitize_eq(s: &str) -> String {
    s.replace('\'', "''")
}

/// Escape single quotes, backslashes, and LIKE wildcards for SQL LIKE patterns.
pub fn sanitize_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Resolve the effective user ID from optional project/user_id overrides.
///
/// Priority: explicit `user_id` > `project` (formatted as `project:<name>`) > `default`.
pub fn resolve_user_id(
    project: &Option<String>,
    user_id: &Option<String>,
    default: &str,
) -> String {
    if let Some(uid) = user_id {
        return uid.clone();
    }
    if let Some(proj) = project {
        return format!("project:{proj}");
    }
    default.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_eq_escapes_single_quotes() {
        assert_eq!(sanitize_eq("it's"), "it''s");
        assert_eq!(sanitize_eq("a''b"), "a''''b");
    }

    #[test]
    fn sanitize_eq_passthrough_clean_input() {
        assert_eq!(sanitize_eq("hello"), "hello");
        assert_eq!(sanitize_eq(""), "");
    }

    #[test]
    fn sanitize_like_escapes_all_special_chars() {
        assert_eq!(sanitize_like("100%"), "100\\%");
        assert_eq!(sanitize_like("a_b"), "a\\_b");
        assert_eq!(sanitize_like("c:\\path"), "c:\\\\path");
        assert_eq!(sanitize_like("it's"), "it''s");
    }

    #[test]
    fn sanitize_like_combined() {
        assert_eq!(sanitize_like("50%_it's\\done"), "50\\%\\_it''s\\\\done");
    }

    #[test]
    fn sanitize_like_passthrough_clean_input() {
        assert_eq!(sanitize_like("hello"), "hello");
        assert_eq!(sanitize_like(""), "");
    }

    #[test]
    fn resolve_user_id_explicit_user_id_wins() {
        let uid = resolve_user_id(&Some("proj".into()), &Some("alice".into()), "default");
        assert_eq!(uid, "alice");
    }

    #[test]
    fn resolve_user_id_project_fallback() {
        let uid = resolve_user_id(&Some("myproj".into()), &None, "default");
        assert_eq!(uid, "project:myproj");
    }

    #[test]
    fn resolve_user_id_default_fallback() {
        let uid = resolve_user_id(&None, &None, "global");
        assert_eq!(uid, "global");
    }

    #[test]
    fn resolve_user_id_empty_strings() {
        let uid = resolve_user_id(&None, &Some("".into()), "default");
        assert_eq!(uid, "");

        let uid = resolve_user_id(&Some("".into()), &None, "default");
        assert_eq!(uid, "project:");
    }
}
