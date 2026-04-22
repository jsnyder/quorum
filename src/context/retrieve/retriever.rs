//! Unified retrieval facade: runs BM25 + vector search in parallel (logically),
//! merges the candidate id sets, fetches full chunks, reranks, filters by
//! score, and returns the top-K.
//!
//! The public entry point is [`Retriever::query`]. Callers own the
//! `Connection` and inject a `Clock` + `Embedder`; everything here is pure
//! SQL + pure ranking.

use std::collections::HashMap;

use rusqlite::Connection;

use super::bm25::{Bm25Hit, bm25_search, build_match_expression};
use super::rerank::{RerankConfig, RerankInput, ScoreBreakdown, rerank};
use super::vector::{VecHit, vec_search};
use super::Filters;
use crate::context::index::traits::{Clock, Embedder};
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

/// Floor on how many candidates we feed into rerank.
const MIN_RETRIEVE: usize = 40;
/// Multiplier over `k` for retrieval over-fetch (rerank sees more candidates).
const RETRIEVE_MULT: usize = 4;

pub struct Retriever<'a, E: Embedder, C: Clock> {
    conn: &'a Connection,
    embedder: &'a E,
    clock: &'a C,
    rerank_config: RerankConfig,
}

impl<'a, E: Embedder, C: Clock> Retriever<'a, E, C> {
    pub fn new(conn: &'a Connection, embedder: &'a E, clock: &'a C) -> Self {
        Self {
            conn,
            embedder,
            clock,
            rerank_config: RerankConfig::default(),
        }
    }

    #[must_use]
    pub fn with_rerank_config(mut self, cfg: RerankConfig) -> Self {
        self.rerank_config = cfg;
        self
    }

    pub fn query(&self, q: RetrievalQuery) -> anyhow::Result<Vec<ScoredChunk>> {
        if q.k == 0 {
            return Ok(Vec::new());
        }

        let k_retrieve = q.k.saturating_mul(RETRIEVE_MULT).max(MIN_RETRIEVE);

        // --- BM25 leg ---
        let fts_expr: Option<String> = if !q.identifiers.is_empty() {
            build_match_expression(&q.identifiers)
        } else if !q.text.trim().is_empty() {
            Some(quote_as_fts_phrase(q.text.trim()))
        } else {
            None
        };

        let bm25_hits: Vec<Bm25Hit> = match &fts_expr {
            Some(expr) => bm25_search(self.conn, expr, &q.filters, k_retrieve)?,
            None => Vec::new(),
        };

        // --- Vector leg ---
        let vec_text: Option<String> = if !q.text.trim().is_empty() {
            Some(q.text.trim().to_string())
        } else if !q.identifiers.is_empty() {
            Some(q.identifiers.join(" "))
        } else {
            None
        };

        let vec_hits: Vec<VecHit> = match vec_text {
            Some(ref t) => {
                let embedding = self.embedder.embed(t);
                vec_search(self.conn, &embedding, &q.filters, k_retrieve)?
            }
            None => Vec::new(),
        };

        if bm25_hits.is_empty() && vec_hits.is_empty() {
            return Ok(Vec::new());
        }

        // --- Merge candidate ids, preserving the best raw signal from each
        // source. sqlite-vec's default metric is cosine distance in [0, 2];
        // map to similarity in [0, 1] via `1 - distance / 2`.
        let mut candidates: HashMap<String, (f32, f32)> = HashMap::new();
        for h in &bm25_hits {
            candidates.entry(h.chunk_id.clone()).or_insert((0.0, 0.0)).0 = h.score;
        }
        for h in &vec_hits {
            let clamped = h.distance.clamp(0.0, 2.0);
            let sim = 1.0 - clamped / 2.0;
            candidates.entry(h.chunk_id.clone()).or_insert((0.0, 0.0)).1 = sim;
        }

        // --- Fetch full chunks by id ---
        let ids: Vec<String> = candidates.keys().cloned().collect();
        let chunks_by_id = fetch_chunks_by_id(self.conn, &ids)?;

        // --- Build rerank inputs (preserve a deterministic order: sort by id). ---
        let mut ordered_ids: Vec<String> = chunks_by_id.keys().cloned().collect();
        ordered_ids.sort();

        let inputs: Vec<RerankInput> = ordered_ids
            .iter()
            .filter_map(|id| {
                let chunk = chunks_by_id.get(id)?;
                let (bm25_raw, vec_raw) = candidates.get(id).copied().unwrap_or((0.0, 0.0));
                let id_exact_match = match &chunk.qualified_name {
                    Some(qn) => q.identifiers.iter().any(|i| i == qn),
                    None => false,
                };
                let language_match = match (&chunk.metadata.language, &q.reviewed_file_language) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                Some(RerankInput {
                    chunk_id: id.clone(),
                    bm25_raw,
                    vec_raw,
                    id_exact_match,
                    language_match,
                    indexed_at: chunk.metadata.indexed_at,
                })
            })
            .collect();

        let scored = rerank(&inputs, self.clock.now(), &self.rerank_config);

        // --- Filter by min_score, sort desc by score (stable tiebreak by id),
        // truncate to k.
        let mut out: Vec<ScoredChunk> = scored
            .into_iter()
            .filter(|(_, br)| br.score >= q.min_score)
            .filter_map(|(id, components)| {
                chunks_by_id.get(&id).map(|chunk| ScoredChunk {
                    chunk: chunk.clone(),
                    score: components.score,
                    components,
                })
            })
            .collect();

        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.chunk.id.cmp(&b.chunk.id))
        });
        out.truncate(q.k);
        Ok(out)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RetrievalQuery {
    /// Free-text query for vector + (fallback) FTS. When identifiers is
    /// empty and text is non-empty, FTS uses the text as a phrase match.
    pub text: String,
    /// Identifier strings for FTS MATCH. When non-empty, FTS uses these
    /// (OR-joined) as the match expression; text is still used for the
    /// embedding query.
    pub identifiers: Vec<String>,
    pub filters: Filters,
    /// Top-K to return after rerank.
    pub k: usize,
    /// Absolute score threshold. Hits with final score < `min_score` are
    /// dropped.
    pub min_score: f32,
    /// The reviewed file's language, used for the language-match boost.
    pub reviewed_file_language: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScoredChunk {
    pub chunk: Chunk,
    pub score: f32,
    pub components: ScoreBreakdown,
}

/// Build an FTS5 phrase match for a free-text query by double-quoting the
/// whole string and escaping embedded double quotes per FTS5 rules.
fn quote_as_fts_phrase(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999; stay comfortably
/// below so the `IN (?, ?, ...)` clause fits under the bind limit.
const ID_BATCH_SIZE: usize = 500;

/// Fetch every chunk whose id is in `ids` and return a lookup map. Unknown
/// ids are silently skipped. Batches the `IN` clause so large candidate
/// sets stay under SQLite's bind-parameter limit.
fn fetch_chunks_by_id(
    conn: &Connection,
    ids: &[String],
) -> rusqlite::Result<HashMap<String, Chunk>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut out = HashMap::with_capacity(ids.len());
    for batch in ids.chunks(ID_BATCH_SIZE) {
        fetch_chunks_batch(conn, batch, &mut out)?;
    }
    Ok(out)
}

fn fetch_chunks_batch(
    conn: &Connection,
    ids: &[String],
    out: &mut HashMap<String, Chunk>,
) -> rusqlite::Result<()> {
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, source, kind, subtype, qualified_name, signature, content,
                source_path, line_start, line_end, commit_sha, indexed_at,
                source_version, language, is_exported, neighboring_symbols,
                extractor, confidence, source_uri
         FROM chunks WHERE id IN ({placeholders})"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), row_to_chunk)?;

    for row in rows {
        let chunk = row?;
        out.insert(chunk.id.clone(), chunk);
    }
    Ok(())
}

fn row_to_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<Chunk> {
    let conv_err = |e: Box<dyn std::error::Error + Send + Sync + 'static>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e)
    };

    let kind_str: String = row.get("kind")?;
    let kind: ChunkKind =
        serde_json::from_str(&format!("\"{kind_str}\"")).map_err(|e| conv_err(Box::new(e)))?;

    let neighbors_json: String = row.get("neighboring_symbols")?;
    let neighbors: Vec<String> =
        serde_json::from_str(&neighbors_json).map_err(|e| conv_err(Box::new(e)))?;

    let indexed_str: String = row.get("indexed_at")?;
    let indexed_at = chrono::DateTime::parse_from_rfc3339(&indexed_str)
        .map(|d| d.with_timezone(&chrono::Utc))
        .map_err(|e| conv_err(Box::new(e)))?;

    let line_start: u32 = row.get("line_start")?;
    let line_end: u32 = row.get("line_end")?;
    let line_range = LineRange::new(line_start, line_end)
        .map_err(|e| conv_err(Box::new(std::io::Error::other(e.to_string()))))?;

    let extractor: String = row.get("extractor")?;
    let confidence: f32 = row.get("confidence")?;
    let source_uri: String = row.get("source_uri")?;
    let provenance = Provenance::new(extractor, confidence, source_uri)
        .map_err(|e| conv_err(Box::new(std::io::Error::other(e.to_string()))))?;

    let is_exported: i64 = row.get("is_exported")?;
    let meta = ChunkMeta {
        source_path: row.get("source_path")?,
        line_range,
        commit_sha: row.get("commit_sha")?,
        indexed_at,
        source_version: row.get("source_version")?,
        language: row.get("language")?,
        is_exported: is_exported != 0,
        neighboring_symbols: neighbors,
    };

    Ok(Chunk {
        id: row.get("id")?,
        source: row.get("source")?,
        kind,
        subtype: row.get("subtype")?,
        qualified_name: row.get("qualified_name")?,
        signature: row.get("signature")?,
        content: row.get("content")?,
        metadata: meta,
        provenance,
    })
}
