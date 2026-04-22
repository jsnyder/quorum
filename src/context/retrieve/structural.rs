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

use rusqlite::{Connection, params_from_iter};

#[derive(Debug, Clone, PartialEq)]
pub struct StructuralHit {
    pub chunk_id: String,
    pub qualified_name: String,
}

/// Case-sensitive equality match on `chunks.qualified_name`. Empty
/// input returns an empty vec without touching the database.
pub fn structural_search(
    conn: &Connection,
    qnames: &[String],
) -> rusqlite::Result<Vec<StructuralHit>> {
    if qnames.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = std::iter::repeat_n("?", qnames.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, qualified_name FROM chunks \
         WHERE qualified_name IS NOT NULL \
         AND qualified_name IN ({placeholders})"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params_from_iter(qnames.iter()), |r| {
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
