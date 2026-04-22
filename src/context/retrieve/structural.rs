//! Structural retrieval over `chunks.qualified_name`.
//!
//! Given a set of qualified names pulled from AST-driven hydration
//! (callees + import targets of the reviewed code), look up the
//! chunks that *define* those symbols. This mirrors "go to
//! definition" in an IDE: the reviewer called `validate`, so the
//! LLM should see the `validate` definition — not another function
//! that merely looks similar.
//!
//! Unlike BM25 / vector which score by relevance, structural hits
//! are either a direct match or they aren't. Ordering of results
//! follows the order of the input qnames for determinism.

use rusqlite::{Connection, ToSql, params_from_iter};

use super::Filters;
use crate::context::types::ChunkKind;

#[derive(Debug, Clone, PartialEq)]
pub struct StructuralHit {
    pub chunk_id: String,
    pub qualified_name: String,
}

/// Case-sensitive equality match on `chunks.qualified_name`, filtered
/// by `filters.sources` / `filters.kinds` / `filters.exclude_source_paths`
/// — the same shape applied to BM25 and vector legs. Empty input
/// returns an empty vec without touching the database.
pub fn structural_search(
    conn: &Connection,
    qnames: &[String],
    filters: &Filters,
) -> rusqlite::Result<Vec<StructuralHit>> {
    if qnames.is_empty() {
        return Ok(Vec::new());
    }

    let qname_ph = std::iter::repeat_n("?", qnames.len())
        .collect::<Vec<_>>()
        .join(",");
    let mut sql = format!(
        "SELECT id, qualified_name FROM chunks \
         WHERE qualified_name IS NOT NULL \
         AND qualified_name IN ({qname_ph})"
    );

    if !filters.sources.is_empty() {
        let ph = std::iter::repeat_n("?", filters.sources.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND source IN ({ph})"));
    }
    if !filters.kinds.is_empty() {
        let ph = std::iter::repeat_n("?", filters.kinds.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND kind IN ({ph})"));
    }
    if !filters.exclude_source_paths.is_empty() {
        let ph = std::iter::repeat_n("?", filters.exclude_source_paths.len())
            .collect::<Vec<_>>()
            .join(",");
        sql.push_str(&format!(" AND source_path NOT IN ({ph})"));
    }

    let mut params: Vec<Box<dyn ToSql>> = Vec::with_capacity(
        qnames.len()
            + filters.sources.len()
            + filters.kinds.len()
            + filters.exclude_source_paths.len(),
    );
    for q in qnames {
        params.push(Box::new(q.clone()));
    }
    for s in &filters.sources {
        params.push(Box::new(s.clone()));
    }
    for k in &filters.kinds {
        params.push(Box::new(kind_to_sql_str(k)));
    }
    for p in &filters.exclude_source_paths {
        params.push(Box::new(p.clone()));
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(params.iter().map(|b| b.as_ref())), |r| {
            Ok(StructuralHit {
                chunk_id: r.get::<_, String>(0)?,
                qualified_name: r.get::<_, String>(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Preserve input order (SQLite `IN` returns rows in arbitrary
    // order; callers rely on determinism for reranking).
    let mut out = Vec::with_capacity(rows.len());
    for qname in qnames {
        for hit in &rows {
            if hit.qualified_name == *qname {
                out.push(hit.clone());
            }
        }
    }
    Ok(out)
}

fn kind_to_sql_str(k: &ChunkKind) -> String {
    match k {
        ChunkKind::Symbol => "symbol".into(),
        ChunkKind::Doc => "doc".into(),
        ChunkKind::Schema => "schema".into(),
    }
}
