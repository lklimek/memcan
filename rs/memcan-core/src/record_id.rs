//! Resolve record IDs given as a full UUID or an unambiguous short prefix.
//!
//! Shared by the TODO and memory tools so both accept the 8-character short
//! IDs they display, and both refuse to guess when a prefix is ambiguous.

use crate::error::{MemcanError, Result};
use crate::query::{sanitize_eq, sanitize_like};
use crate::traits::VectorStore;

/// Minimum prefix length before scanning: eight hex characters provide a 32-bit collision-resistance floor.
pub const MIN_ID_PREFIX_LEN: usize = 8;
/// Caps ambiguous candidates listed before the error says "more than N".
pub const MAX_ID_PREFIX_MATCHES: usize = 5;

/// Kind of record an ID names, for error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    Todo,
    Memory,
}

impl RecordKind {
    /// Singular noun, e.g. `todo`.
    pub fn singular(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Memory => "memory",
        }
    }

    /// Plural noun, e.g. `todos`.
    pub fn plural(self) -> &'static str {
        match self {
            Self::Todo => "todos",
            Self::Memory => "memories",
        }
    }
}

/// Describe the candidates of an ambiguous prefix for its error message.
pub(crate) fn describe_matches(kind: RecordKind, matches: &[String], truncated: bool) -> String {
    if truncated {
        format!(
            "more than {MAX_ID_PREFIX_MATCHES} {} (showing {MAX_ID_PREFIX_MATCHES}: {})",
            kind.plural(),
            matches.join(", ")
        )
    } else {
        format!(
            "{} {} ({})",
            matches.len(),
            kind.plural(),
            matches.join(", ")
        )
    }
}

/// Canonical form of a caller-supplied ID: trimmed and lowercased, since
/// generated UUIDs are stored lowercase.
fn normalize(id: &str) -> String {
    id.trim().to_lowercase()
}

/// Resolve `id` as an exact ID first, then as a short-ID prefix. The prefix
/// match ignores surrounding whitespace and case.
///
/// `user_id` limits the prefix match to records of that `user_id` scope (for
/// tables with that column); an exact ID matches in any scope.
///
/// Returns `Ok(None)` when nothing matches (including a prefix shorter than
/// [`MIN_ID_PREFIX_LEN`]) and [`MemcanError::AmbiguousIdPrefix`] when a
/// prefix matches more than one record.
pub async fn resolve_id(
    store: &dyn VectorStore,
    table: &str,
    kind: RecordKind,
    id: &str,
    user_id: Option<&str>,
) -> Result<Option<String>> {
    let exact = store.get(table, &[id.to_string()]).await?;
    if let Some(record) = exact.first() {
        return Ok(Some(record.id.clone()));
    }
    resolve_id_prefix(store, table, kind, id, user_id).await
}

/// Resolve `prefix` to the single record ID that starts with it.
///
/// Same contract as [`resolve_id`], without the exact-ID lookup.
pub async fn resolve_id_prefix(
    store: &dyn VectorStore,
    table: &str,
    kind: RecordKind,
    prefix: &str,
    user_id: Option<&str>,
) -> Result<Option<String>> {
    let prefix = normalize(prefix);
    if prefix.chars().count() < MIN_ID_PREFIX_LEN {
        return Ok(None);
    }

    let mut filter = format!("id LIKE '{}%'", sanitize_like(&prefix));
    if let Some(user_id) = user_id {
        filter.push_str(&format!(" AND user_id = '{}'", sanitize_eq(user_id)));
    }
    let candidates = store
        .scroll(table, Some(&filter), MAX_ID_PREFIX_MATCHES + 1, 0)
        .await?;
    let mut candidate_ids: Vec<_> = candidates
        .into_iter()
        .map(|candidate| candidate.id)
        .collect();
    candidate_ids.sort();
    candidate_ids.dedup();

    match candidate_ids.len() {
        0 => Ok(None),
        1 => Ok(candidate_ids.pop()),
        count => {
            let truncated = count > MAX_ID_PREFIX_MATCHES;
            candidate_ids.truncate(MAX_ID_PREFIX_MATCHES);
            Err(MemcanError::AmbiguousIdPrefix {
                kind,
                prefix,
                matches: candidate_ids,
                truncated,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_prefix_messages_name_the_kind_and_ask_for_the_full_uuid() {
        let error = MemcanError::AmbiguousIdPrefix {
            kind: RecordKind::Memory,
            prefix: "2e22752c".into(),
            matches: vec!["2e22752c-a".into(), "2e22752c-b".into()],
            truncated: false,
        };
        assert_eq!(
            error.to_string(),
            "ambiguous memory id prefix '2e22752c' matches 2 memories \
             (2e22752c-a, 2e22752c-b) — provide the full UUID"
        );

        let truncated = MemcanError::AmbiguousIdPrefix {
            kind: RecordKind::Todo,
            prefix: "2e22752c".into(),
            matches: vec!["2e22752c-a".into(); MAX_ID_PREFIX_MATCHES],
            truncated: true,
        };
        let message = truncated.to_string();
        assert!(
            message.contains("matches more than 5 todos (showing 5:"),
            "{message}"
        );
    }
}
