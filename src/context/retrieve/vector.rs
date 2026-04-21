//! Vector KNN search over sqlite-vec's `chunks_vec` virtual table.
//!
//! Filter strategy: vec0 rejects JOINs combined with `MATCH + k`, so we
//! over-fetch `k * 4` ids from vec0 first, then intersect against `chunks`
//! with the requested source/kind filters in a second query.

use rusqlite::{Connection, params_from_iter};

use crate::context::types::ChunkKind;

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub sources: Vec<String>,
    pub kinds: Vec<ChunkKind>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VecHit {
    pub chunk_id: String,
    pub distance: f32,
}

/// Serialize the vec0 `k` hyperparameter.
const OVERFETCH_MULTIPLIER: usize = 4;

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
    let overfetch = k.saturating_mul(OVERFETCH_MULTIPLIER).max(k);

    // Step 1: pull the nearest `overfetch` ids from vec0 (no JOIN).
    let mut stmt = conn.prepare(
        "SELECT id, distance
         FROM chunks_vec
         WHERE embedding MATCH ?1 AND k = ?2
         ORDER BY distance",
    )?;

    let rows: Vec<(String, f32)> = stmt
        .query_map(rusqlite::params![q_bytes, overfetch as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f32>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    // Step 2: apply source/kind filters against `chunks` and preserve vec0
    // distance order.
    let no_filters = filters.sources.is_empty() && filters.kinds.is_empty();

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

        let mut params: Vec<String> = rows.iter().map(|(id, _)| id.clone()).collect();
        params.extend(filters.sources.iter().cloned());
        params.extend(filters.kinds.iter().map(kind_to_sql_str));

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
