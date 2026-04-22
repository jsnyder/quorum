//! BM25 retrieval over the `chunks_fts` FTS5 virtual table.
//!
//! FTS5's built-in `bm25()` ranking function returns a negative score where
//! lower (more negative) values indicate stronger relevance. Downstream code
//! (rerank, fusion) prefers a "higher = better" convention, so we negate.

use rusqlite::{Connection, ToSql, params_from_iter};

use super::Filters;
use crate::context::types::ChunkKind;

/// Single BM25 hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Hit {
    pub chunk_id: String,
    /// Higher = more relevant (FTS5's native rank is negated here).
    pub score: f32,
}

/// Run a BM25 query against `chunks_fts`, joined with `chunks` for
/// source/kind filters. Returns the top-`k` hits in descending relevance.
///
/// The `query` is passed verbatim to FTS5. Use [`build_match_expression`] to
/// safely quote identifier-style terms.
pub fn bm25_search(
    conn: &Connection,
    query: &str,
    filters: &Filters,
    k: usize,
) -> rusqlite::Result<Vec<Bm25Hit>> {
    if query.trim().is_empty() || k == 0 {
        return Ok(Vec::new());
    }

    let sql = build_query_sql(
        filters.sources.len(),
        filters.kinds.len(),
        filters.exclude_source_paths.len(),
    );

    let mut params: Vec<Box<dyn ToSql>> = Vec::new();
    params.push(Box::new(query.to_string()));
    for s in &filters.sources {
        params.push(Box::new(s.clone()));
    }
    for kind in &filters.kinds {
        params.push(Box::new(kind_to_sql_string(kind)));
    }
    for path in &filters.exclude_source_paths {
        params.push(Box::new(path.clone()));
    }
    params.push(Box::new(k as i64));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter().map(|b| b.as_ref())), |r| {
        let id: String = r.get(0)?;
        let rank: f64 = r.get(1)?;
        Ok(Bm25Hit {
            chunk_id: id,
            score: -rank as f32,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Build a safe FTS5 MATCH expression from a list of identifier strings.
/// Each non-empty identifier is double-quoted (with embedded quotes doubled
/// per FTS5 rules) and joined with `OR`. Returns `None` when every input is
/// empty/whitespace.
pub fn build_match_expression(identifiers: &[String]) -> Option<String> {
    let parts: Vec<String> = identifiers
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" OR "))
    }
}

fn build_query_sql(source_count: usize, kind_count: usize, exclude_path_count: usize) -> String {
    let source_clause = if source_count == 0 {
        "1=1".to_string()
    } else {
        let placeholders = vec!["?"; source_count].join(",");
        format!("c.source IN ({placeholders})")
    };
    let kind_clause = if kind_count == 0 {
        "1=1".to_string()
    } else {
        let placeholders = vec!["?"; kind_count].join(",");
        format!("c.kind IN ({placeholders})")
    };
    let exclude_clause = if exclude_path_count == 0 {
        "1=1".to_string()
    } else {
        let placeholders = vec!["?"; exclude_path_count].join(",");
        format!("c.source_path NOT IN ({placeholders})")
    };
    format!(
        "SELECT f.id, bm25(chunks_fts) AS rank \
         FROM chunks_fts AS f \
         JOIN chunks AS c ON c.id = f.id \
         WHERE chunks_fts MATCH ?1 AND {source_clause} AND {kind_clause} AND {exclude_clause} \
         ORDER BY rank \
         LIMIT ?"
    )
}

fn kind_to_sql_string(kind: &ChunkKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}
