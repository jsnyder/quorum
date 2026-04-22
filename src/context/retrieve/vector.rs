//! Vector KNN search over sqlite-vec's `chunks_vec` virtual table.
//!
//! Filter strategy: vec0 rejects JOINs combined with `MATCH + k`, so we
//! over-fetch ids from vec0 first, then intersect against `chunks` with the
//! requested source/kind filters in a second query. The overfetch size
//! doubles (up to `k * 32`) when filters drop the candidate set below `k`.

use rusqlite::{Connection, params_from_iter};

use super::Filters;
use crate::context::types::ChunkKind;

#[derive(Debug, Clone, PartialEq)]
pub struct VecHit {
    pub chunk_id: String,
    pub distance: f32,
}

/// Initial overfetch multiplier before filters are applied.
const OVERFETCH_MULTIPLIER: usize = 4;
/// Cap on adaptive overfetch growth.
const OVERFETCH_CAP_MULTIPLIER: usize = 32;
/// sqlite-vec's compile-time KNN `k` limit.
const VEC_K_HARD_LIMIT: usize = 4096;

pub fn vec_search(
    conn: &Connection,
    q_embedding: &[f32],
    filters: &Filters,
    k: usize,
) -> rusqlite::Result<Vec<VecHit>> {
    if k == 0 {
        return Ok(Vec::new());
    }

    let q_bytes = embedding_to_le_bytes(q_embedding);
    let mut fetch = k
        .saturating_mul(OVERFETCH_MULTIPLIER)
        .max(k)
        .min(VEC_K_HARD_LIMIT);
    let cap = k
        .saturating_mul(OVERFETCH_CAP_MULTIPLIER)
        .max(k)
        .min(VEC_K_HARD_LIMIT);

    loop {
        let rows = run_vec_query(conn, &q_bytes, fetch)?;
        let raw_len = rows.len();

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let filtered = apply_filters(conn, rows, filters, k)?;

        // Enough results OR the index returned fewer rows than we asked for
        // (index exhausted) OR we've reached the cap.
        if filtered.len() >= k || raw_len < fetch || fetch >= cap {
            return Ok(filtered);
        }

        fetch = fetch.saturating_mul(2).min(cap);
    }
}

fn run_vec_query(
    conn: &Connection,
    q_bytes: &[u8],
    fetch: usize,
) -> rusqlite::Result<Vec<(String, f32)>> {
    let mut stmt = conn.prepare(
        "SELECT id, distance
         FROM chunks_vec
         WHERE embedding MATCH ?1 AND k = ?2
         ORDER BY distance",
    )?;

    stmt.query_map(rusqlite::params![q_bytes, fetch as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, f32>(1)?))
    })?
    .collect::<rusqlite::Result<_>>()
}

fn apply_filters(
    conn: &Connection,
    rows: Vec<(String, f32)>,
    filters: &Filters,
    k: usize,
) -> rusqlite::Result<Vec<VecHit>> {
    let no_filters = filters.sources.is_empty()
        && filters.kinds.is_empty()
        && filters.exclude_source_paths.is_empty();

    let allowed_ids: std::collections::HashSet<String> = if no_filters {
        rows.iter().map(|(id, _)| id.clone()).collect()
    } else {
        let id_placeholders =
            std::iter::repeat_n("?", rows.len()).collect::<Vec<_>>().join(",");

        let mut sql = format!("SELECT id FROM chunks WHERE id IN ({id_placeholders})");

        if !filters.sources.is_empty() {
            let src_placeholders = std::iter::repeat_n("?", filters.sources.len())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND source IN ({src_placeholders})"));
        }
        if !filters.kinds.is_empty() {
            let kind_placeholders = std::iter::repeat_n("?", filters.kinds.len())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND kind IN ({kind_placeholders})"));
        }
        if !filters.exclude_source_paths.is_empty() {
            let excl_placeholders = std::iter::repeat_n("?", filters.exclude_source_paths.len())
                .collect::<Vec<_>>()
                .join(",");
            sql.push_str(&format!(" AND source_path NOT IN ({excl_placeholders})"));
        }

        let mut params: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
        params.extend(filters.sources.iter().cloned());
        params.extend(filters.kinds.iter().map(kind_to_sql_str));
        params.extend(filters.exclude_source_paths.iter().cloned());

        let mut q = conn.prepare(&sql)?;
        q.query_map(params_from_iter(params.iter()), |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?
    };

    let out: Vec<VecHit> = rows
        .into_iter()
        .filter(|(id, _)| allowed_ids.contains(id))
        .take(k)
        .map(|(chunk_id, distance)| VecHit { chunk_id, distance })
        .collect();

    Ok(out)
}

pub(crate) fn embedding_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn kind_to_sql_str(k: &ChunkKind) -> String {
    match k {
        ChunkKind::Symbol => "symbol".into(),
        ChunkKind::Doc => "doc".into(),
        ChunkKind::Schema => "schema".into(),
    }
}
