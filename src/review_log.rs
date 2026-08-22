//! Per-review telemetry log: one JSON line per `quorum review` invocation.
//!
//! Recorded to `~/.quorum/reviews.jsonl` to enable dimensional analytics
//! (by-repo, by-caller, rolling trend) in `quorum stats`. Joins to
//! `feedback.jsonl` / `calibrator_traces.jsonl` via `run_id` (ULID).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::finding::Severity;
use crate::storage::StorageHandle;

/// Lightweight per-finding metadata written alongside `ReviewRecord` to the
/// `review_finding_ids` table. Carries the finding's title and originating
/// file path so downstream analytics (stats, feedback joins) can display
/// context without re-parsing the full review output.
pub struct FindingMeta {
    pub id: String,
    pub title: String,
    pub file_path: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SeverityCounts {
    #[serde(default)]
    pub critical: u32,
    #[serde(default)]
    pub high: u32,
    #[serde(default)]
    pub medium: u32,
    #[serde(default)]
    pub low: u32,
    #[serde(default)]
    pub info: u32,
}

impl SeverityCounts {
    pub fn from_severities<'a, I: IntoIterator<Item = &'a Severity>>(iter: I) -> Self {
        let mut s = Self::default();
        for sev in iter {
            match sev {
                Severity::Critical => s.critical = s.critical.saturating_add(1),
                Severity::High => s.high = s.high.saturating_add(1),
                Severity::Medium => s.medium = s.medium.saturating_add(1),
                Severity::Low => s.low = s.low.saturating_add(1),
                Severity::Info => s.info = s.info.saturating_add(1),
            }
        }
        s
    }

    pub fn total(&self) -> u32 {
        self.critical
            .saturating_add(self.high)
            .saturating_add(self.medium)
            .saturating_add(self.low)
            .saturating_add(self.info)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Flags {
    #[serde(default)]
    pub deep: bool,
    #[serde(default)]
    pub parallel_n: u32,
    #[serde(default)]
    pub ensemble: bool,
}

/// Outcome of the `quorum context` retrieve→plan→render pipeline for a
/// single review invocation.
///
/// When no injector was wired into the pipeline (the default), callers
/// still record a default-constructed [`ContextTelemetry`] (all zeros /
/// `false` / empty) so dashboards can distinguish "no injector" from
/// "injector produced nothing".
///
/// Backwards-compatibility: the `context` field on [`ReviewRecord`] uses
/// `#[serde(default)]`, so legacy records written before this block
/// existed still deserialize cleanly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContextTelemetry {
    /// True iff `sources.context.auto_inject` was enabled for this review.
    #[serde(default)]
    pub auto_inject_enabled: bool,
    /// True iff an injector was wired into the pipeline at all.
    #[serde(default)]
    pub injector_available: bool,
    /// True iff the retriever closure returned an error for this review
    /// (dashboards can distinguish "retriever crashed" from "no hits").
    #[serde(default)]
    pub retriever_errored: bool,
    /// Total chunks returned by the retriever (pre-plan filtering).
    #[serde(default)]
    pub retrieved_chunk_count: u32,
    /// Chunks that ended up in the rendered block.
    #[serde(default)]
    pub injected_chunk_count: u32,
    /// Token cost of the injected chunks (as counted by the planner).
    #[serde(default)]
    pub injected_tokens: u32,
    /// Candidates whose score fell below the effective threshold.
    #[serde(default)]
    pub below_threshold_count: u32,
    /// True iff the planner lowered the prose threshold adaptively.
    #[serde(default)]
    pub adaptive_threshold_applied: bool,
    /// Prose threshold actually applied (may differ from `inject_min_score`
    /// when adaptive lowering kicked in).
    #[serde(default)]
    pub effective_prose_threshold: f32,
    /// Chunk IDs in emission order.
    #[serde(default)]
    pub injected_chunk_ids: Vec<String>,
    /// Unique source names represented in the injected chunks.
    #[serde(default)]
    pub injected_sources: Vec<String>,
    /// Count of precedence winner-records (deduped qualified_names).
    #[serde(default)]
    pub precedence_entries: u32,
    /// Cumulative retrieve+plan+render wall time for this invocation.
    #[serde(default)]
    pub render_duration_ms: u64,
    /// sha256 of the rendered context block. `None` when no block was
    /// injected.
    #[serde(default)]
    pub rendered_prompt_hash: Option<String>,
    /// Chunks dropped post-retrieve by the calibrator's per-chunk
    /// `injection_threshold_for` gate (raised thresholds from prior
    /// `Verdict::ContextMisleading` feedback). `0` when no calibrator was
    /// wired into the injector.
    #[serde(default)]
    pub suppressed_by_calibrator: u32,
    /// Chunks dropped post-retrieve by the global `inject_min_score` floor.
    /// Split out from `suppressed_by_calibrator` so dashboards can tell
    /// "config rejected it" apart from "feedback poisoned this chunk".
    #[serde(default)]
    pub suppressed_by_floor: u32,
    /// Chunks dropped before any gating because their rerank score was
    /// NaN. A nonzero value means the retriever or rerank produced
    /// invalid floats — an upstream bug — and we stripped them so the
    /// downstream comparisons wouldn't silently misbehave.
    #[serde(default)]
    pub nan_scores_dropped: u32,
    /// Per-leg breakdown of the candidate pool BEFORE top-K truncation.
    /// Answers: "how often does each leg surface hits at all?"
    #[serde(default)]
    pub retrieved_by_leg: LegCounts,
    /// Per-leg breakdown of the chunks that survived to the final
    /// rendered block. Answers: "do structural-only hits actually
    /// appear in the LLM prompt, or are they always outranked?"
    #[serde(default)]
    pub injected_by_leg: LegCounts,
    /// Minimum rerank score across all retrieved chunks. Together with
    /// p10 this paints the lower tail of the distribution so tau can
    /// be raised with confidence rather than guesswork.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score_min: Option<f32>,
    /// 10th percentile of retrieved rerank scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score_p10: Option<f32>,
    /// Median of the rerank scores across all retrieved chunks, before
    /// any filtering. Pair with p90 to see whether `inject_min_score`
    /// is actually binding — if tau sits below the median, it never
    /// bites.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score_median: Option<f32>,
    /// 90th percentile of retrieved rerank scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_score_p90: Option<f32>,

    // --- Multi-source telemetry (v0.23.0+) ---
    /// Number of source DBs queried in this review.
    #[serde(default)]
    pub sources_queried: u32,
    /// Number of sources with at least one chunk in the final top-k.
    #[serde(default)]
    pub sources_contributing: u32,
    /// Per-source chunk count in the final injected set.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub per_source_contributions: std::collections::BTreeMap<String, u32>,
    /// Chunks that received the dep-manifest boost.
    #[serde(default)]
    pub dep_boost_applied: u32,
    /// Reserved slots available for the current repo.
    #[serde(default)]
    pub current_repo_reserved_available: u32,
    /// Reserved slots actually filled by current-repo chunks.
    #[serde(default)]
    pub current_repo_reserved_filled: u32,

    // --- Structural fingerprint telemetry ---
    /// How many chunks received structural fingerprints at index time.
    #[serde(default)]
    pub structural_fingerprints_computed: u32,
    /// Number of structural KNN queries performed during retrieval.
    #[serde(default)]
    pub structural_knn_queries: u32,
    /// Total results returned from structural KNN queries.
    #[serde(default)]
    pub structural_knn_hits: u32,
    /// How many reranked results received a nonzero structural boost.
    #[serde(default)]
    pub structural_boost_applied: u32,
}

/// Count of chunks attributed to each retrieval leg, plus a
/// `total_unique` that counts each chunk once regardless of how many
/// legs surfaced it. `structural_only` is the slice of `structural`
/// whose chunks were surfaced by NO other leg — the headline signal
/// for "is structural retrieval adding unique value?"
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegCounts {
    #[serde(default)]
    pub bm25: u32,
    #[serde(default)]
    pub vector: u32,
    #[serde(default)]
    pub structural: u32,
    #[serde(default)]
    pub structural_only: u32,
    #[serde(default)]
    pub total_unique: u32,
}

impl LegCounts {
    /// Saturating per-field sum. Used by the per-run aggregator in
    /// `main.rs` to combine per-file telemetry into a single record.
    /// `total_unique` naively sums — when the same chunk appears in
    /// two files' reviews this double-counts, which is the right
    /// behavior for a "how much context got injected across the whole
    /// review" measurement.
    pub fn saturating_add(&mut self, rhs: &LegCounts) {
        self.bm25 = self.bm25.saturating_add(rhs.bm25);
        self.vector = self.vector.saturating_add(rhs.vector);
        self.structural = self.structural.saturating_add(rhs.structural);
        self.structural_only = self.structural_only.saturating_add(rhs.structural_only);
        self.total_unique = self.total_unique.saturating_add(rhs.total_unique);
    }

    /// Aggregate counts across a slice where each element exposes its
    /// `source_legs` as a slice of [`RetrievalLeg`].
    pub fn from_chunks<T>(chunks: &[T]) -> Self
    where
        T: AsRef<[crate::context::retrieve::retriever::RetrievalLeg]>,
    {
        use crate::context::retrieve::retriever::RetrievalLeg;
        let mut c = LegCounts::default();
        for ch in chunks {
            let legs = ch.as_ref();
            if legs.is_empty() {
                continue;
            }
            c.total_unique = c.total_unique.saturating_add(1);
            let has_b = legs.contains(&RetrievalLeg::Bm25);
            let has_v = legs.contains(&RetrievalLeg::Vector);
            let has_s = legs.contains(&RetrievalLeg::Structural);
            if has_b {
                c.bm25 = c.bm25.saturating_add(1);
            }
            if has_v {
                c.vector = c.vector.saturating_add(1);
            }
            if has_s {
                c.structural = c.structural.saturating_add(1);
                if !has_b && !has_v {
                    c.structural_only = c.structural_only.saturating_add(1);
                }
            }
        }
        c
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub quorum_version: String,
    pub repo: Option<String>,
    pub invoked_from: String,
    pub model: String,
    pub files_reviewed: u32,
    pub lines_added: Option<u32>,
    pub lines_removed: Option<u32>,
    pub findings_by_severity: SeverityCounts,
    #[serde(default)]
    pub suppressed_by_rule: HashMap<String, u32>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(default)]
    pub tokens_cache_read: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub flags: Flags,
    /// Review mode (plan, docs). Omitted for code reviews (the default)
    /// so legacy records without this field deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Context-injection telemetry for this invocation. Defaults to a
    /// semantic-zero [`ContextTelemetry`] when no injector was wired.
    /// Marked `#[serde(default)]` for backwards-compat with records
    /// written before this field existed.
    #[serde(default)]
    pub context: ContextTelemetry,

    /// Stable per-finding ULIDs emitted by this review (one per finding,
    /// in stable post-suppression order). Empty for legacy records that
    /// pre-date this field. Used by stats analytics to join feedback
    /// entries (`FeedbackEntry.finding_id`) back to their originating
    /// review for per-finding precision deduplication.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finding_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills_used: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_findings: Option<HashMap<String, u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrator_findings_out: Option<u32>,
}

impl ReviewRecord {
    pub fn new_ulid() -> String {
        Ulid::new().to_string()
    }
}

/// Internal backend discriminator: JSONL (legacy) or SQLite.
enum Backend {
    Jsonl(PathBuf),
    Sqlite(StorageHandle),
}

/// Intermediate row extracted from SQLite. All columns are pulled inside the
/// `query_map` closure (where the `Row` borrow lives), then converted to
/// `ReviewRecord` after the statement is dropped.
struct RawReviewRow {
    run_id: String,
    ts_str: String,
    quorum_version: String,
    repo: Option<String>,
    invoked_from: String,
    model: String,
    files_reviewed: i64,
    lines_added: Option<i64>,
    lines_removed: Option<i64>,
    critical: i64,
    high: i64,
    medium: i64,
    low: i64,
    info: i64,
    suppressed_json: String,
    tokens_in: i64,
    tokens_out: i64,
    tokens_cache_read: i64,
    duration_ms: i64,
    flag_deep: i32,
    flag_parallel_n: i32,
    flag_ensemble: i32,
    mode: Option<String>,
    context_json: String,
}

impl RawReviewRow {
    /// Convert a raw SQLite row into a `ReviewRecord`, looking up
    /// finding_ids from the pre-loaded map.
    fn into_record(
        self,
        finding_map: &mut HashMap<String, Vec<String>>,
    ) -> anyhow::Result<ReviewRecord> {
        let timestamp = DateTime::parse_from_rfc3339(&self.ts_str)
            .with_context(|| {
                format!(
                    "invalid timestamp in review {}: {}",
                    self.run_id, self.ts_str
                )
            })?
            .with_timezone(&Utc);

        let suppressed_by_rule: HashMap<String, u32> = serde_json::from_str(&self.suppressed_json)
            .with_context(|| {
                format!(
                    "invalid suppressed_by_rule JSON in review {}: {}",
                    self.run_id, self.suppressed_json
                )
            })?;

        let context: ContextTelemetry =
            serde_json::from_str(&self.context_json).with_context(|| {
                format!(
                    "invalid context JSON in review {}: {}",
                    self.run_id, self.context_json
                )
            })?;

        let finding_ids = finding_map.remove(&self.run_id).unwrap_or_default();

        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let record = ReviewRecord {
            run_id: self.run_id,
            timestamp,
            quorum_version: self.quorum_version,
            repo: self.repo,
            invoked_from: self.invoked_from,
            model: self.model,
            files_reviewed: self.files_reviewed as u32,
            lines_added: self.lines_added.map(|v| v as u32),
            lines_removed: self.lines_removed.map(|v| v as u32),
            findings_by_severity: SeverityCounts {
                critical: self.critical as u32,
                high: self.high as u32,
                medium: self.medium as u32,
                low: self.low as u32,
                info: self.info as u32,
            },
            suppressed_by_rule,
            tokens_in: self.tokens_in as u64,
            tokens_out: self.tokens_out as u64,
            tokens_cache_read: self.tokens_cache_read as u64,
            duration_ms: self.duration_ms as u64,
            flags: Flags {
                deep: self.flag_deep != 0,
                parallel_n: self.flag_parallel_n as u32,
                ensemble: self.flag_ensemble != 0,
            },
            mode: self.mode,
            context,
            finding_ids,
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        };
        Ok(record)
    }
}

pub struct ReviewLog {
    backend: Backend,
}

impl ReviewLog {
    /// Create a ReviewLog backed by a JSONL file.
    /// Retained for backward compatibility, migration support, and tests.
    /// New callers should use `with_storage()`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            backend: Backend::Jsonl(path),
        }
    }

    /// Create a SQLite-backed review log.
    pub fn with_storage(handle: StorageHandle) -> Self {
        Self {
            backend: Backend::Sqlite(handle),
        }
    }

    /// Path to the underlying JSONL file. Returns an empty path for
    /// the SQLite backend (only used for display purposes).
    pub fn path(&self) -> &Path {
        match &self.backend {
            Backend::Jsonl(p) => p,
            Backend::Sqlite(_) => Path::new(""),
        }
    }

    /// Stream records line-by-line from the JSONL log, skipping malformed lines.
    /// Returns an empty iterator if the file does not exist.
    ///
    /// Only supported for the JSONL backend. The SQLite backend returns an
    /// empty iterator (callers should use `load_all()` instead).
    pub fn iter(&self) -> anyhow::Result<ReviewLogIter> {
        match &self.backend {
            Backend::Jsonl(path) => {
                use std::fs::File;
                use std::io::{BufRead, BufReader};
                if !path.exists() {
                    return Ok(ReviewLogIter { inner: None });
                }
                let file = File::open(path)
                    .with_context(|| format!("Failed to open review log: {}", path.display()))?;
                let reader: Box<dyn BufRead> = Box::new(BufReader::new(file));
                Ok(ReviewLogIter {
                    inner: Some(reader.lines()),
                })
            }
            Backend::Sqlite(_) => Ok(ReviewLogIter { inner: None }),
        }
    }

    /// Convenience: collect all records (suitable for small logs and tests).
    pub fn load_all(&self) -> anyhow::Result<Vec<ReviewRecord>> {
        match &self.backend {
            Backend::Jsonl(_) => self.iter()?.collect(),
            Backend::Sqlite(handle) => Self::load_all_sqlite(handle),
        }
    }

    /// Load the most recent `n` records in chronological order (oldest first).
    ///
    /// The SQLite backend uses `ORDER BY timestamp DESC LIMIT ?` for an
    /// efficient partial scan, then reverses into chronological order.
    /// The JSONL backend falls back to `load_all()` and takes the tail.
    ///
    /// Returns an empty `Vec` when `n == 0`.
    pub fn load_recent(&self, n: usize) -> anyhow::Result<Vec<ReviewRecord>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        match &self.backend {
            Backend::Jsonl(_) => {
                let all = self.load_all()?;
                let start = all.len().saturating_sub(n);
                Ok(all[start..].to_vec())
            }
            Backend::Sqlite(handle) => Self::load_recent_sqlite(handle, n),
        }
    }

    /// Return all distinct finding_ids across every review without
    /// deserializing full `ReviewRecord` structs (SQLite path).
    /// The JSONL path falls back to `load_all()`.
    pub fn load_all_finding_ids(&self) -> anyhow::Result<std::collections::HashSet<String>> {
        match &self.backend {
            Backend::Jsonl(_) => {
                let all = self.load_all()?;
                Ok(all.into_iter().flat_map(|r| r.finding_ids).collect())
            }
            Backend::Sqlite(handle) => Self::load_all_finding_ids_sqlite(handle),
        }
    }

    fn load_all_finding_ids_sqlite(
        handle: &StorageHandle,
    ) -> anyhow::Result<std::collections::HashSet<String>> {
        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let mut stmt = conn.prepare("SELECT DISTINCT finding_id FROM review_finding_ids")?;
        let ids: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<_, _>>()?;
        Ok(ids)
    }

    /// Load all records with `timestamp >= since` in chronological order.
    ///
    /// The SQLite backend uses a `WHERE timestamp >= ?1` clause for an
    /// efficient filtered scan. The JSONL backend falls back to
    /// `load_all()` and filters in Rust.
    pub fn load_since(&self, since: DateTime<Utc>) -> anyhow::Result<Vec<ReviewRecord>> {
        match &self.backend {
            Backend::Jsonl(_) => {
                let all = self.load_all()?;
                Ok(all.into_iter().filter(|r| r.timestamp >= since).collect())
            }
            Backend::Sqlite(handle) => Self::load_since_sqlite(handle, since),
        }
    }

    /// Return the total number of review records without deserializing them.
    ///
    /// The SQLite backend uses `SELECT COUNT(*)` for an O(1) scan.
    /// The JSONL backend falls back to `load_all().len()`.
    pub fn count(&self) -> anyhow::Result<usize> {
        match &self.backend {
            Backend::Jsonl(_) => Ok(self.load_all()?.len()),
            Backend::Sqlite(handle) => {
                let conn = handle
                    .lock()
                    .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM reviews", [], |row| row.get(0))?;
                Ok(count as usize)
            }
        }
    }

    /// Append one record. Creates the file (and parent dir) if missing (JSONL),
    /// or inserts into the `reviews` + `review_finding_ids` tables (SQLite).
    pub fn record(&self, entry: &ReviewRecord) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Jsonl(path) => Self::record_jsonl(path, entry),
            Backend::Sqlite(handle) => Self::record_sqlite(handle, entry),
        }
    }

    /// Append one record with enriched per-finding metadata (title, file_path).
    ///
    /// The JSONL backend ignores `meta` (finding metadata is embedded in the
    /// serialized record's `finding_ids`). The SQLite backend writes the
    /// `title` and `file_path` columns into `review_finding_ids`.
    pub fn record_with_meta(
        &self,
        entry: &ReviewRecord,
        meta: &[FindingMeta],
    ) -> anyhow::Result<()> {
        match &self.backend {
            Backend::Jsonl(path) => Self::record_jsonl(path, entry),
            Backend::Sqlite(handle) => Self::record_sqlite_with_meta(handle, entry, meta),
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn normalize_title(s: &str) -> String {
        s.replace(['`', '*'], "").replace('_', " ").to_lowercase()
    }

    // ── Finding-ID resolution ──────────────────────────────────────────

    pub fn resolve_finding_id(&self, file_path: &str, finding_title: &str) -> Option<String> {
        use std::sync::LazyLock;
        static STOP_WORDS: LazyLock<std::collections::HashSet<&'static str>> =
            LazyLock::new(|| {
                [
                    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "for",
                    "from", "has", "have", "in", "is", "it", "its", "may", "might", "not", "of",
                    "on", "or", "should", "that", "the", "this", "to", "was", "were", "will",
                    "with", "would",
                ]
                .into_iter()
                .collect()
            });

        let Backend::Sqlite(handle) = &self.backend else {
            return None;
        };
        let conn = handle.lock().ok()?;

        let norm_path = file_path.strip_prefix("./").unwrap_or(file_path);
        let path_variant = format!("./{norm_path}");

        let mut stmt = conn
            .prepare(
                "SELECT rfi.finding_id, rfi.title
                 FROM review_finding_ids rfi
                 JOIN reviews r ON r.run_id = rfi.run_id
                 WHERE (rfi.file_path = ?1 OR rfi.file_path = ?2)
                   AND rfi.title <> ''
                 ORDER BY r.timestamp DESC
                 LIMIT 200",
            )
            .ok()?;
        let candidates: Vec<(String, String)> = stmt
            .query_map(rusqlite::params![norm_path, path_variant], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .ok()?
            .filter_map(|r| r.ok())
            .collect();

        let query_norm = Self::normalize_title(finding_title);
        let query_words: std::collections::HashSet<&str> = query_norm
            .split_whitespace()
            .filter(|w| !STOP_WORDS.contains(w))
            .collect();

        if query_words.is_empty() {
            return None;
        }

        let mut best_id: Option<String> = None;
        let mut best_score: f64 = 0.0;

        for (fid, title) in &candidates {
            let title_norm = Self::normalize_title(title);
            let title_words: std::collections::HashSet<&str> = title_norm
                .split_whitespace()
                .filter(|w| !STOP_WORDS.contains(w))
                .collect();

            let intersection = query_words.intersection(&title_words).count();
            let union = query_words.union(&title_words).count();
            let jaccard = if union > 0 {
                intersection as f64 / union as f64
            } else {
                0.0
            };

            let min_size = query_words.len().min(title_words.len());
            let containment = if min_size > 0 {
                intersection as f64 / min_size as f64
            } else {
                0.0
            };

            let substring_bonus =
                if title_norm.contains(&query_norm) || query_norm.contains(&title_norm) {
                    0.3
                } else {
                    0.0
                };

            let score = (0.4 * jaccard + 0.6 * containment + substring_bonus).min(1.0);
            if score > best_score {
                best_score = score;
                best_id = Some(fid.clone());
            }
        }

        if best_score >= 0.4 {
            tracing::debug!(
                finding_id = best_id.as_deref().unwrap_or(""),
                score = best_score,
                file = file_path,
                "auto-linked feedback to finding"
            );
            best_id
        } else {
            tracing::info!(
                file = file_path,
                title = finding_title,
                best_score = best_score,
                "no auto-link match found for feedback"
            );
            None
        }
    }

    // ── JSONL backend ──────────────────────────────────────────────────

    fn record_jsonl(path: &Path, entry: &ReviewRecord) -> anyhow::Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create review log dir: {}", parent.display())
            })?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("Failed to open review log: {}", path.display()))?;
        let mut buf = serde_json::to_string(entry)?;
        buf.push('\n');
        file.write_all(buf.as_bytes())?;
        Ok(())
    }

    // ── SQLite backend ─────────────────────────────────────────────────

    fn record_sqlite(handle: &StorageHandle, entry: &ReviewRecord) -> anyhow::Result<()> {
        use rusqlite::params;

        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        let tx = conn.unchecked_transaction()?;

        let context_json = serde_json::to_string(&entry.context)?;
        let suppressed_json = serde_json::to_string(&entry.suppressed_by_rule)?;
        let ts = entry.timestamp.to_rfc3339();

        // u64 -> i64 casts are intentional: SQLite INTEGER is signed 64-bit.
        // Token counts and durations are well within i64 range in practice.
        #[allow(clippy::cast_possible_wrap)]
        tx.execute(
            "INSERT INTO reviews (
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble,
                mode, context
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15,
                ?16, ?17, ?18, ?19,
                ?20, ?21, ?22,
                ?23, ?24
            )",
            params![
                entry.run_id,
                ts,
                entry.quorum_version,
                entry.repo,
                entry.invoked_from,
                entry.model,
                entry.files_reviewed,
                entry.lines_added.map(i64::from),
                entry.lines_removed.map(i64::from),
                i64::from(entry.findings_by_severity.critical),
                i64::from(entry.findings_by_severity.high),
                i64::from(entry.findings_by_severity.medium),
                i64::from(entry.findings_by_severity.low),
                i64::from(entry.findings_by_severity.info),
                suppressed_json,
                entry.tokens_in as i64,
                entry.tokens_out as i64,
                entry.tokens_cache_read as i64,
                entry.duration_ms as i64,
                i32::from(entry.flags.deep),
                i64::from(entry.flags.parallel_n),
                i32::from(entry.flags.ensemble),
                entry.mode,
                context_json,
            ],
        )?;

        for fid in &entry.finding_ids {
            tx.execute(
                "INSERT INTO review_finding_ids (run_id, finding_id) VALUES (?1, ?2)",
                params![entry.run_id, fid],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Like `record_sqlite` but writes enriched finding metadata (title,
    /// file_path) into the `review_finding_ids` child table instead of
    /// bare finding_ids from the entry.
    fn record_sqlite_with_meta(
        handle: &StorageHandle,
        entry: &ReviewRecord,
        meta: &[FindingMeta],
    ) -> anyhow::Result<()> {
        use rusqlite::params;

        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;
        let tx = conn.unchecked_transaction()?;

        let context_json = serde_json::to_string(&entry.context)?;
        let suppressed_json = serde_json::to_string(&entry.suppressed_by_rule)?;
        let ts = entry.timestamp.to_rfc3339();

        #[allow(clippy::cast_possible_wrap)]
        tx.execute(
            "INSERT INTO reviews (
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble,
                mode, context
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9,
                ?10, ?11, ?12, ?13, ?14,
                ?15,
                ?16, ?17, ?18, ?19,
                ?20, ?21, ?22,
                ?23, ?24
            )",
            params![
                entry.run_id,
                ts,
                entry.quorum_version,
                entry.repo,
                entry.invoked_from,
                entry.model,
                entry.files_reviewed,
                entry.lines_added.map(i64::from),
                entry.lines_removed.map(i64::from),
                i64::from(entry.findings_by_severity.critical),
                i64::from(entry.findings_by_severity.high),
                i64::from(entry.findings_by_severity.medium),
                i64::from(entry.findings_by_severity.low),
                i64::from(entry.findings_by_severity.info),
                suppressed_json,
                entry.tokens_in as i64,
                entry.tokens_out as i64,
                entry.tokens_cache_read as i64,
                entry.duration_ms as i64,
                i32::from(entry.flags.deep),
                i64::from(entry.flags.parallel_n),
                i32::from(entry.flags.ensemble),
                entry.mode,
                context_json,
            ],
        )?;

        for fm in meta {
            tx.execute(
                "INSERT INTO review_finding_ids (run_id, finding_id, title, file_path) VALUES (?1, ?2, ?3, ?4)",
                params![entry.run_id, fm.id, fm.title, fm.file_path],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    fn load_all_sqlite(handle: &StorageHandle) -> anyhow::Result<Vec<ReviewRecord>> {
        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        // Pre-load all finding_ids grouped by run_id.
        let mut finding_map: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt =
                conn.prepare("SELECT run_id, finding_id FROM review_finding_ids ORDER BY rowid")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (run_id, finding_id) = row?;
                finding_map.entry(run_id).or_default().push(finding_id);
            }
        }

        let raw_rows = Self::query_raw_rows(&conn)?;

        let mut records = Vec::with_capacity(raw_rows.len());
        for r in raw_rows {
            records.push(r.into_record(&mut finding_map)?);
        }
        Ok(records)
    }

    /// Execute the SELECT query and extract all columns into `RawReviewRow`
    /// structs, keeping the rusqlite `Row` borrow confined to the closure.
    fn query_raw_rows(conn: &rusqlite::Connection) -> anyhow::Result<Vec<RawReviewRow>> {
        let mut stmt = conn.prepare(
            "SELECT
                run_id, timestamp, quorum_version, repo, invoked_from, model,
                files_reviewed, lines_added, lines_removed,
                critical, high, medium, low, info,
                suppressed_by_rule,
                tokens_in, tokens_out, tokens_cache_read, duration_ms,
                flag_deep, flag_parallel_n, flag_ensemble,
                mode, context
            FROM reviews
            ORDER BY timestamp ASC",
        )?;

        let raw_rows: Vec<RawReviewRow> = stmt
            .query_map([], |row| {
                Ok(RawReviewRow {
                    run_id: row.get(0)?,
                    ts_str: row.get(1)?,
                    quorum_version: row.get(2)?,
                    repo: row.get(3)?,
                    invoked_from: row.get(4)?,
                    model: row.get(5)?,
                    files_reviewed: row.get(6)?,
                    lines_added: row.get(7)?,
                    lines_removed: row.get(8)?,
                    critical: row.get(9)?,
                    high: row.get(10)?,
                    medium: row.get(11)?,
                    low: row.get(12)?,
                    info: row.get(13)?,
                    suppressed_json: row.get(14)?,
                    tokens_in: row.get(15)?,
                    tokens_out: row.get(16)?,
                    tokens_cache_read: row.get(17)?,
                    duration_ms: row.get(18)?,
                    flag_deep: row.get(19)?,
                    flag_parallel_n: row.get(20)?,
                    flag_ensemble: row.get(21)?,
                    mode: row.get(22)?,
                    context_json: row.get(23)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(raw_rows)
    }

    /// Load at most `n` recent reviews. Queries the DB with
    /// `ORDER BY timestamp DESC LIMIT ?`, pre-loads only the matching
    /// finding_ids, then reverses to chronological order.
    fn load_recent_sqlite(handle: &StorageHandle, n: usize) -> anyhow::Result<Vec<ReviewRecord>> {
        use rusqlite::params;

        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        // 1. Identify the run_ids we need (DESC, capped at n).
        let run_ids: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT run_id FROM reviews ORDER BY timestamp DESC, run_id DESC LIMIT ?1",
            )?;
            stmt.query_map(params![n as i64], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?
        };

        if run_ids.is_empty() {
            return Ok(Vec::new());
        }

        // 2. Pre-load finding_ids only for these run_ids.
        let mut finding_map: HashMap<String, Vec<String>> = HashMap::new();
        {
            let placeholders: String = run_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT run_id, finding_id FROM review_finding_ids \
                 WHERE run_id IN ({placeholders}) ORDER BY rowid"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(run_ids.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (run_id, finding_id) = row?;
                finding_map.entry(run_id).or_default().push(finding_id);
            }
        }

        // 3. Fetch full rows (DESC, LIMIT n).
        let raw_rows: Vec<RawReviewRow> = {
            let mut stmt = conn.prepare(
                "SELECT
                    run_id, timestamp, quorum_version, repo, invoked_from, model,
                    files_reviewed, lines_added, lines_removed,
                    critical, high, medium, low, info,
                    suppressed_by_rule,
                    tokens_in, tokens_out, tokens_cache_read, duration_ms,
                    flag_deep, flag_parallel_n, flag_ensemble,
                    mode, context
                FROM reviews
                ORDER BY timestamp DESC, run_id DESC
                LIMIT ?1",
            )?;
            stmt.query_map(params![n as i64], |row| {
                Ok(RawReviewRow {
                    run_id: row.get(0)?,
                    ts_str: row.get(1)?,
                    quorum_version: row.get(2)?,
                    repo: row.get(3)?,
                    invoked_from: row.get(4)?,
                    model: row.get(5)?,
                    files_reviewed: row.get(6)?,
                    lines_added: row.get(7)?,
                    lines_removed: row.get(8)?,
                    critical: row.get(9)?,
                    high: row.get(10)?,
                    medium: row.get(11)?,
                    low: row.get(12)?,
                    info: row.get(13)?,
                    suppressed_json: row.get(14)?,
                    tokens_in: row.get(15)?,
                    tokens_out: row.get(16)?,
                    tokens_cache_read: row.get(17)?,
                    duration_ms: row.get(18)?,
                    flag_deep: row.get(19)?,
                    flag_parallel_n: row.get(20)?,
                    flag_ensemble: row.get(21)?,
                    mode: row.get(22)?,
                    context_json: row.get(23)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        // 4. Convert and reverse to chronological (ASC) order.
        let mut records = Vec::with_capacity(raw_rows.len());
        for r in raw_rows {
            records.push(r.into_record(&mut finding_map)?);
        }
        records.reverse();
        Ok(records)
    }

    /// Load reviews with `timestamp >= since`. Uses a SQL WHERE clause
    /// for efficient filtering and pre-loads only the matching finding_ids
    /// via a JOIN.
    fn load_since_sqlite(
        handle: &StorageHandle,
        since: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ReviewRecord>> {
        use rusqlite::params;

        let conn = handle
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))?;

        let since_str = since.to_rfc3339();

        // 1. Pre-load finding_ids for matching reviews via JOIN.
        let mut finding_map: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT f.run_id, f.finding_id
                 FROM review_finding_ids f
                 INNER JOIN reviews r ON r.run_id = f.run_id
                 WHERE r.timestamp >= ?1
                 ORDER BY f.rowid",
            )?;
            let rows = stmt.query_map(params![since_str], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (run_id, finding_id) = row?;
                finding_map.entry(run_id).or_default().push(finding_id);
            }
        }

        // 2. Fetch full rows matching the time filter.
        let raw_rows: Vec<RawReviewRow> = {
            let mut stmt = conn.prepare(
                "SELECT
                    run_id, timestamp, quorum_version, repo, invoked_from, model,
                    files_reviewed, lines_added, lines_removed,
                    critical, high, medium, low, info,
                    suppressed_by_rule,
                    tokens_in, tokens_out, tokens_cache_read, duration_ms,
                    flag_deep, flag_parallel_n, flag_ensemble,
                    mode, context
                FROM reviews
                WHERE timestamp >= ?1
                ORDER BY timestamp ASC",
            )?;
            stmt.query_map(params![since_str], |row| {
                Ok(RawReviewRow {
                    run_id: row.get(0)?,
                    ts_str: row.get(1)?,
                    quorum_version: row.get(2)?,
                    repo: row.get(3)?,
                    invoked_from: row.get(4)?,
                    model: row.get(5)?,
                    files_reviewed: row.get(6)?,
                    lines_added: row.get(7)?,
                    lines_removed: row.get(8)?,
                    critical: row.get(9)?,
                    high: row.get(10)?,
                    medium: row.get(11)?,
                    low: row.get(12)?,
                    info: row.get(13)?,
                    suppressed_json: row.get(14)?,
                    tokens_in: row.get(15)?,
                    tokens_out: row.get(16)?,
                    tokens_cache_read: row.get(17)?,
                    duration_ms: row.get(18)?,
                    flag_deep: row.get(19)?,
                    flag_parallel_n: row.get(20)?,
                    flag_ensemble: row.get(21)?,
                    mode: row.get(22)?,
                    context_json: row.get(23)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        };

        // 3. Convert to ReviewRecords.
        let mut records = Vec::with_capacity(raw_rows.len());
        for r in raw_rows {
            records.push(r.into_record(&mut finding_map)?);
        }
        Ok(records)
    }
}

/// Streaming iterator over a reviews.jsonl file.
/// Malformed lines are logged to stderr and skipped — parity with FeedbackStore.
pub struct ReviewLogIter {
    inner: Option<std::io::Lines<Box<dyn std::io::BufRead>>>,
}

impl Iterator for ReviewLogIter {
    type Item = anyhow::Result<ReviewRecord>;

    fn next(&mut self) -> Option<Self::Item> {
        let lines = self.inner.as_mut()?;
        loop {
            match lines.next()? {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<ReviewRecord>(&line) {
                    Ok(rec) => return Some(Ok(rec)),
                    Err(e) => {
                        eprintln!("warning: skipping malformed review record: {}", e);
                        continue;
                    }
                },
                Err(e) => return Some(Err(anyhow::anyhow!("read error: {}", e))),
            }
        }
    }
}

/// Detect invocation context from env vars. Mirrors the detection used for
/// compact-mode sniffing in telemetry.rs. Priority order matters: more specific
/// signals beat generic `AGENT`.
pub fn detect_invoked_from(caller_override: Option<&str>) -> String {
    if let Some(name) = caller_override
        && !name.is_empty()
    {
        return name.to_string();
    }
    if std::env::var_os("CLAUDE_CODE").is_some() {
        return "claude_code".to_string();
    }
    if std::env::var_os("CODEX_CI").is_some() {
        return "codex_ci".to_string();
    }
    if std::env::var_os("GEMINI_CLI").is_some() {
        return "gemini_cli".to_string();
    }
    if let Some(v) = std::env::var_os("AGENT")
        && let Some(s) = v.to_str()
        && !s.is_empty()
    {
        return s.to_string();
    }
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        "tty".to_string()
    } else {
        "pipe".to_string()
    }
}

/// Walk parents of the given path looking for a `.git` directory.
/// Returns the basename of the repo root, or None if not in a git repo.
pub fn detect_repo(start: &Path) -> Option<String> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(start)
    };
    let mut cur: &Path = &start;
    loop {
        if cur.join(".git").exists() {
            return cur.file_name().and_then(|s| s.to_str()).map(String::from);
        }
        cur = cur.parent()?;
    }
}

/// Deterministic sha256 hex digest of a rendered context block. Used as
/// the `rendered_prompt_hash` field in [`ContextTelemetry`] so dashboards
/// can group reruns that produced byte-identical blocks.
pub fn hash_rendered_block(rendered: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(rendered.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_record() -> ReviewRecord {
        let mut suppressed = HashMap::new();
        suppressed.insert("tautological-length".into(), 2);
        ReviewRecord {
            run_id: ReviewRecord::new_ulid(),
            timestamp: Utc::now(),
            quorum_version: env!("CARGO_PKG_VERSION").to_string(),
            repo: Some("quorum".into()),
            invoked_from: "tty".into(),
            model: "gpt-5.4".into(),
            files_reviewed: 3,
            lines_added: Some(120),
            lines_removed: Some(40),
            findings_by_severity: SeverityCounts {
                critical: 1,
                high: 2,
                medium: 0,
                low: 0,
                info: 4,
            },
            suppressed_by_rule: suppressed,
            tokens_in: 12_345,
            tokens_out: 678,
            tokens_cache_read: 8_000,
            duration_ms: 4_200,
            flags: Flags {
                deep: false,
                parallel_n: 4,
                ensemble: false,
            },
            mode: None,
            context: ContextTelemetry::default(),
            finding_ids: Vec::new(),
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        }
    }

    #[test]
    fn record_round_trips_through_json() {
        let rec = sample_record();
        let json = serde_json::to_string(&rec).unwrap();
        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec, back);
    }

    // ─── Stats redesign Phase 0: ReviewRecord.finding_ids ───

    #[test]
    fn legacy_review_record_deserializes_with_empty_finding_ids() {
        // Pre-rollout reviews.jsonl rows lack the finding_ids key. They must
        // load cleanly (default empty Vec) so the linkage diagnostic counts
        // them as "unlinked legacy" rather than failing the read.
        let legacy = r#"{"run_id":"01ABC","timestamp":"2026-01-01T00:00:00Z","quorum_version":"0.1","repo":null,"invoked_from":"tty","model":"gpt","files_reviewed":1,"lines_added":null,"lines_removed":null,"findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},"tokens_in":0,"tokens_out":0,"duration_ms":0}"#;
        let rec: ReviewRecord = serde_json::from_str(legacy).expect("legacy load");
        assert_eq!(rec.finding_ids, Vec::<String>::new());
    }

    #[test]
    fn record_with_finding_ids_round_trips_preserving_order() {
        // Order matters: linkage_stats joins by ID, but downstream consumers
        // (rule attribution, time-ordering) may want positional access.
        let mut rec = sample_record();
        rec.finding_ids = vec![
            "01HXYZ0000000000000000000A".into(),
            "01HXYZ0000000000000000000B".into(),
            "01HXYZ0000000000000000000C".into(),
        ];
        let json = serde_json::to_string(&rec).unwrap();
        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(rec.finding_ids, back.finding_ids);
    }

    #[test]
    fn record_omits_finding_ids_key_when_empty() {
        // Disk-bloat regression: don't write `"finding_ids":[]` for every
        // legacy-style record produced by code that doesn't yet populate.
        let rec = sample_record();
        assert!(rec.finding_ids.is_empty(), "fixture default must be empty");
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("finding_ids"),
            "empty finding_ids must not write the key: {json}"
        );
    }

    #[test]
    fn ulid_is_26_chars_and_unique() {
        let a = ReviewRecord::new_ulid();
        let b = ReviewRecord::new_ulid();
        assert_eq!(a.len(), 26);
        assert_eq!(b.len(), 26);
        assert_ne!(a, b);
    }

    #[test]
    fn writer_creates_and_appends() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reviews.jsonl");
        let log = ReviewLog::new(path.clone());
        log.record(&sample_record()).unwrap();
        log.record(&sample_record()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in lines {
            let _: ReviewRecord = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn writer_creates_missing_parent_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested/sub/reviews.jsonl");
        let log = ReviewLog::new(path.clone());
        log.record(&sample_record()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn severity_counts_from_iter() {
        let sevs = [
            Severity::Critical,
            Severity::High,
            Severity::High,
            Severity::Info,
            Severity::Info,
            Severity::Info,
        ];
        let sc = SeverityCounts::from_severities(sevs.iter());
        assert_eq!(sc.critical, 1);
        assert_eq!(sc.high, 2);
        assert_eq!(sc.info, 3);
        assert_eq!(sc.total(), 6);
    }

    #[test]
    fn invoked_from_override_wins() {
        let got = detect_invoked_from(Some("my-script"));
        assert_eq!(got, "my-script");
    }

    #[test]
    fn invoked_from_claude_code_env() {
        // Serialize env-var tests so concurrent tests don't race on env state.
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var_os("CLAUDE_CODE");
        unsafe { std::env::set_var("CLAUDE_CODE", "1") };
        let got = detect_invoked_from(None);
        match prev {
            Some(v) => unsafe { std::env::set_var("CLAUDE_CODE", v) },
            None => unsafe { std::env::remove_var("CLAUDE_CODE") },
        }
        assert_eq!(got, "claude_code");
    }

    #[test]
    fn invoked_from_agent_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prev_claude = std::env::var_os("CLAUDE_CODE");
        let prev_codex = std::env::var_os("CODEX_CI");
        let prev_gemini = std::env::var_os("GEMINI_CLI");
        let prev_agent = std::env::var_os("AGENT");
        unsafe {
            std::env::remove_var("CLAUDE_CODE");
            std::env::remove_var("CODEX_CI");
            std::env::remove_var("GEMINI_CLI");
            std::env::set_var("AGENT", "cursor");
        }
        let got = detect_invoked_from(None);
        // Restore
        unsafe {
            if let Some(v) = prev_claude {
                std::env::set_var("CLAUDE_CODE", v);
            }
            if let Some(v) = prev_codex {
                std::env::set_var("CODEX_CI", v);
            }
            if let Some(v) = prev_gemini {
                std::env::set_var("GEMINI_CLI", v);
            }
            match prev_agent {
                Some(v) => std::env::set_var("AGENT", v),
                None => std::env::remove_var("AGENT"),
            }
        }
        assert_eq!(got, "cursor");
    }

    #[test]
    fn detect_repo_finds_git_root() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        let sub = dir.path().join("src/nested");
        std::fs::create_dir_all(&sub).unwrap();
        let got = detect_repo(&sub).unwrap();
        let expected = dir
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(got, expected);
    }

    #[test]
    fn detect_repo_returns_none_for_filesystem_root() {
        // Root has no parent with a .git directory (on any reasonable system).
        // Using "/" guarantees we exhaust the parent chain without a match.
        let got = detect_repo(Path::new("/"));
        assert!(
            got.is_none(),
            "filesystem root should yield None, got {:?}",
            got
        );
    }

    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn iter_over_empty_path_yields_nothing() {
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("absent.jsonl"));
        let n = log.iter().unwrap().count();
        assert_eq!(n, 0);
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        let mut ids = Vec::new();
        for _ in 0..5 {
            let mut r = sample_record();
            r.run_id = ReviewRecord::new_ulid();
            ids.push(r.run_id.clone());
            log.record(&r).unwrap();
        }
        let got: Vec<String> = log.iter().unwrap().map(|r| r.unwrap().run_id).collect();
        assert_eq!(got, ids);
    }

    #[test]
    fn iter_skips_malformed_lines() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("reviews.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", serde_json::to_string(&sample_record()).unwrap()).unwrap();
            writeln!(f, "{{ this is not json").unwrap();
            writeln!(f).unwrap();
            writeln!(f, "{}", serde_json::to_string(&sample_record()).unwrap()).unwrap();
        }
        let log = ReviewLog::new(path);
        let records: Vec<_> = log.iter().unwrap().filter_map(|r| r.ok()).collect();
        assert_eq!(
            records.len(),
            2,
            "should skip malformed + blank, keep valid"
        );
    }

    #[test]
    fn load_all_round_trips_many_records() {
        // Smoke test that many records can be written and re-read.
        // Verifies streaming path works for larger inputs.
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        for _ in 0..1_000 {
            log.record(&sample_record()).unwrap();
        }
        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 1_000);
    }

    // ---- ContextTelemetry (Task 6.2) ------------------------------------

    fn populated_context_telemetry() -> ContextTelemetry {
        ContextTelemetry {
            auto_inject_enabled: true,
            injector_available: true,
            retriever_errored: false,
            retrieved_chunk_count: 5,
            injected_chunk_count: 2,
            injected_tokens: 180,
            below_threshold_count: 3,
            adaptive_threshold_applied: false,
            effective_prose_threshold: 0.65,
            injected_chunk_ids: vec!["chunk-a".into(), "chunk-b".into()],
            injected_sources: vec!["mini-rust".into()],
            precedence_entries: 1,
            render_duration_ms: 42,
            rendered_prompt_hash: Some("deadbeef".into()),
            suppressed_by_calibrator: 0,
            suppressed_by_floor: 0,
            nan_scores_dropped: 2,
            retrieved_by_leg: super::LegCounts::default(),
            injected_by_leg: super::LegCounts::default(),
            rerank_score_min: Some(0.41),
            rerank_score_p10: Some(0.55),
            rerank_score_median: Some(0.72),
            rerank_score_p90: Some(0.88),
            sources_queried: 1,
            sources_contributing: 1,
            per_source_contributions: std::collections::BTreeMap::new(),
            dep_boost_applied: 0,
            current_repo_reserved_available: 0,
            current_repo_reserved_filled: 0,
            structural_fingerprints_computed: 0,
            structural_knn_queries: 0,
            structural_knn_hits: 0,
            structural_boost_applied: 0,
        }
    }

    #[test]
    fn context_telemetry_populated_after_successful_injection() {
        // Simulates the pipeline handing the review-log a non-default
        // ContextTelemetry after a successful injection pass. The record
        // must serialize with every telemetry field present.
        let mut rec = sample_record();
        rec.context = populated_context_telemetry();

        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            json.contains("\"auto_inject_enabled\":true"),
            "json: {json}"
        );
        assert!(json.contains("\"injector_available\":true"));
        assert!(json.contains("\"retrieved_chunk_count\":5"));
        assert!(json.contains("\"injected_chunk_count\":2"));
        assert!(json.contains("\"injected_tokens\":180"));
        assert!(json.contains("\"below_threshold_count\":3"));
        assert!(json.contains("\"adaptive_threshold_applied\":false"));
        assert!(json.contains("\"effective_prose_threshold\":0.65"));
        assert!(json.contains("\"chunk-a\""));
        assert!(json.contains("\"chunk-b\""));
        assert!(json.contains("\"mini-rust\""));
        assert!(json.contains("\"precedence_entries\":1"));
        assert!(json.contains("\"render_duration_ms\":42"));
        assert!(json.contains("\"rendered_prompt_hash\":\"deadbeef\""));

        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.context, populated_context_telemetry());
    }

    #[test]
    fn context_telemetry_is_semantic_zeros_when_no_injector_wired() {
        // When no injector is wired, the pipeline writes a default
        // ContextTelemetry: auto_inject_enabled=false, everything else
        // 0/empty/false. The record round-trips cleanly.
        let rec = sample_record();
        assert!(!rec.context.auto_inject_enabled);
        assert!(!rec.context.injector_available);
        assert_eq!(rec.context.retrieved_chunk_count, 0);
        assert_eq!(rec.context.injected_chunk_count, 0);
        assert_eq!(rec.context.injected_tokens, 0);
        assert_eq!(rec.context.below_threshold_count, 0);
        assert!(!rec.context.adaptive_threshold_applied);
        assert_eq!(rec.context.effective_prose_threshold, 0.0);
        assert!(rec.context.injected_chunk_ids.is_empty());
        assert!(rec.context.injected_sources.is_empty());
        assert_eq!(rec.context.precedence_entries, 0);
        assert_eq!(rec.context.render_duration_ms, 0);
        assert!(rec.context.rendered_prompt_hash.is_none());

        // End-to-end: record must write + reload identically.
        let dir = TempDir::new().unwrap();
        let log = ReviewLog::new(dir.path().join("reviews.jsonl"));
        log.record(&rec).unwrap();
        let back = log.load_all().unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].context, ContextTelemetry::default());
    }

    #[test]
    fn structural_fingerprint_telemetry_defaults_and_round_trip() {
        // 1. Default construction: all new fields are 0.
        let default_ctx = ContextTelemetry::default();
        assert_eq!(default_ctx.structural_fingerprints_computed, 0);
        assert_eq!(default_ctx.structural_knn_queries, 0);
        assert_eq!(default_ctx.structural_knn_hits, 0);
        assert_eq!(default_ctx.structural_boost_applied, 0);

        // 2. Populated round-trip: set nonzero values, serialize, deserialize.
        let ctx = ContextTelemetry {
            structural_fingerprints_computed: 42,
            structural_knn_queries: 3,
            structural_knn_hits: 17,
            structural_boost_applied: 5,
            ..ContextTelemetry::default()
        };

        let json = serde_json::to_string(&ctx).unwrap();
        assert!(json.contains("\"structural_fingerprints_computed\":42"));
        assert!(json.contains("\"structural_knn_queries\":3"));
        assert!(json.contains("\"structural_knn_hits\":17"));
        assert!(json.contains("\"structural_boost_applied\":5"));

        let back: ContextTelemetry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ctx);

        // 3. Backward compat: old JSON without the new fields -> 0.
        let old_json = r#"{
            "auto_inject_enabled": true,
            "injector_available": true,
            "retrieved_chunk_count": 10
        }"#;
        let old: ContextTelemetry = serde_json::from_str(old_json)
            .expect("old JSON without structural fields must deserialize");
        assert_eq!(old.structural_fingerprints_computed, 0);
        assert_eq!(old.structural_knn_queries, 0);
        assert_eq!(old.structural_knn_hits, 0);
        assert_eq!(old.structural_boost_applied, 0);
        // Existing fields still parse correctly.
        assert!(old.auto_inject_enabled);
        assert!(old.injector_available);
        assert_eq!(old.retrieved_chunk_count, 10);
    }

    #[test]
    fn legacy_review_record_without_context_field_deserializes() {
        // Legacy JSON line written by quorum <= v0.15.x (no `context`
        // field). Must deserialize, with `context` defaulted to zeros.
        let legacy = r#"{
            "run_id":"01HX000000000000000000000X",
            "timestamp":"2026-04-20T12:00:00Z",
            "quorum_version":"0.15.0",
            "repo":"legacy-repo",
            "invoked_from":"tty",
            "model":"gpt-5.4",
            "files_reviewed":1,
            "lines_added":null,
            "lines_removed":null,
            "findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},
            "tokens_in":100,
            "tokens_out":20,
            "duration_ms":500
        }"#;
        let rec: ReviewRecord = serde_json::from_str(legacy)
            .expect("legacy record without `context` field must deserialize");
        assert_eq!(rec.context, ContextTelemetry::default());
        assert_eq!(rec.run_id, "01HX000000000000000000000X");
        assert_eq!(rec.repo.as_deref(), Some("legacy-repo"));
    }

    #[test]
    fn context_telemetry_hash_is_stable_across_reruns() {
        // Same rendered string → same hash. Different string → different
        // hash. Guards against accidental use of a non-deterministic
        // hasher or per-run salt.
        use super::hash_rendered_block;
        let a = hash_rendered_block("# Context\n\n## mini-rust\n\nfoo");
        let b = hash_rendered_block("# Context\n\n## mini-rust\n\nfoo");
        let c = hash_rendered_block("# Context\n\n## mini-rust\n\nbar");
        assert_eq!(a, b, "deterministic hasher must agree across calls");
        assert_ne!(a, c, "distinct inputs must produce distinct hashes");
        assert_eq!(a.len(), 64, "sha256 hex digest is 64 chars");
    }

    mod leg_counts {
        use super::super::LegCounts;
        use crate::context::retrieve::retriever::RetrievalLeg;

        fn legs(tags: &[RetrievalLeg]) -> Vec<RetrievalLeg> {
            tags.to_vec()
        }

        #[test]
        fn empty_input_produces_zero_counts() {
            let counts = LegCounts::from_chunks::<Vec<RetrievalLeg>>(&[]);
            assert_eq!(counts.bm25, 0);
            assert_eq!(counts.vector, 0);
            assert_eq!(counts.structural, 0);
            assert_eq!(counts.structural_only, 0);
            assert_eq!(counts.total_unique, 0);
        }

        #[test]
        fn single_leg_chunks_count_once_per_leg() {
            let chunks = vec![
                legs(&[RetrievalLeg::Bm25]),
                legs(&[RetrievalLeg::Vector]),
                legs(&[RetrievalLeg::Structural]),
            ];
            let c = LegCounts::from_chunks(&chunks);
            assert_eq!(c.bm25, 1);
            assert_eq!(c.vector, 1);
            assert_eq!(c.structural, 1);
            assert_eq!(
                c.structural_only, 1,
                "lone Structural tag is structural_only"
            );
            assert_eq!(c.total_unique, 3);
        }

        #[test]
        fn multi_leg_chunk_increments_each_leg_but_total_unique_once() {
            let chunks = vec![legs(&[RetrievalLeg::Bm25, RetrievalLeg::Structural])];
            let c = LegCounts::from_chunks(&chunks);
            assert_eq!(c.bm25, 1);
            assert_eq!(c.vector, 0);
            assert_eq!(c.structural, 1);
            assert_eq!(
                c.structural_only, 0,
                "Structural+Bm25 is NOT structural_only"
            );
            assert_eq!(
                c.total_unique, 1,
                "multi-leg chunk counts once toward total_unique"
            );
        }

        #[test]
        fn structural_only_partition_invariant() {
            let chunks = vec![
                legs(&[RetrievalLeg::Structural]),
                legs(&[RetrievalLeg::Structural]),
                legs(&[RetrievalLeg::Bm25, RetrievalLeg::Structural]),
                legs(&[RetrievalLeg::Vector, RetrievalLeg::Structural]),
            ];
            let c = LegCounts::from_chunks(&chunks);
            let with_others = c.structural - c.structural_only;
            assert_eq!(c.structural, 4);
            assert_eq!(c.structural_only, 2);
            assert_eq!(with_others, 2);
            assert_eq!(c.structural_only + with_others, c.structural);
        }

        #[test]
        fn per_leg_counts_never_exceed_total_unique() {
            let chunks = vec![
                legs(&[RetrievalLeg::Bm25, RetrievalLeg::Vector]),
                legs(&[RetrievalLeg::Bm25]),
                legs(&[RetrievalLeg::Structural]),
            ];
            let c = LegCounts::from_chunks(&chunks);
            assert_eq!(c.total_unique, 3);
            assert!(c.bm25 <= c.total_unique);
            assert!(c.vector <= c.total_unique);
            assert!(c.structural <= c.total_unique);
        }
    }

    // ---- mode field (Task 6) -----------------------------------------------

    #[test]
    fn mode_serializes_when_present() {
        let mut rec = sample_record();
        rec.mode = Some("plan".into());
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains(r#""mode":"plan""#), "json: {json}");
        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mode.as_deref(), Some("plan"));
    }

    #[test]
    fn mode_omitted_for_code_reviews() {
        let rec = sample_record(); // mode is None
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("\"mode\""),
            "mode should be skip_serialized when None; json: {json}"
        );
    }

    #[test]
    fn mode_defaults_to_none_for_legacy_records() {
        // Legacy JSON line written before the mode field existed.
        let legacy = r#"{
            "run_id":"01HX000000000000000000000X",
            "timestamp":"2026-04-20T12:00:00Z",
            "quorum_version":"0.15.0",
            "repo":"legacy-repo",
            "invoked_from":"tty",
            "model":"gpt-5.4",
            "files_reviewed":1,
            "lines_added":null,
            "lines_removed":null,
            "findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},
            "tokens_in":100,
            "tokens_out":20,
            "duration_ms":500
        }"#;
        let rec: ReviewRecord = serde_json::from_str(legacy)
            .expect("legacy record without mode field must deserialize");
        assert!(
            rec.mode.is_none(),
            "mode should default to None for legacy records"
        );
    }

    // ── SQLite backend tests ──────────────────────────────────────────

    /// Fixture builder for test `ReviewRecord`s. Reduces duplication
    /// across SQLite tests — callers override only what they need.
    fn test_review_record(run_id: &str) -> ReviewRecord {
        ReviewRecord {
            run_id: run_id.to_string(),
            timestamp: Utc::now(),
            quorum_version: "0.22.0".to_string(),
            repo: None,
            invoked_from: "test".to_string(),
            model: "test-model".to_string(),
            files_reviewed: 1,
            lines_added: None,
            lines_removed: None,
            findings_by_severity: SeverityCounts::default(),
            suppressed_by_rule: HashMap::new(),
            tokens_in: 0,
            tokens_out: 0,
            tokens_cache_read: 0,
            duration_ms: 0,
            flags: Flags::default(),
            mode: None,
            context: ContextTelemetry::default(),
            finding_ids: vec![],
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        }
    }

    /// Helper: create a SQLite-backed ReviewLog in a temp directory.
    fn sqlite_review_log(dir: &TempDir) -> ReviewLog {
        let handle = crate::storage::initialize(dir.path()).expect("storage init");
        ReviewLog::with_storage(handle)
    }

    #[test]
    fn sqlite_record_round_trip() {
        // Full record with all fields populated; verify every field
        // survives a write-then-read cycle through SQLite.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);

        let mut suppressed = HashMap::new();
        suppressed.insert("tautological-length".into(), 2u32);
        suppressed.insert("bare-except-pass".into(), 1u32);

        let rec = ReviewRecord {
            run_id: "01TEST_FULL_ROUNDTRIP00001".to_string(),
            timestamp: chrono::DateTime::parse_from_rfc3339("2026-05-10T14:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            quorum_version: "0.22.0".to_string(),
            repo: Some("quorum".to_string()),
            invoked_from: "claude_code".to_string(),
            model: "gpt-5.4".to_string(),
            files_reviewed: 7,
            lines_added: Some(120),
            lines_removed: Some(40),
            findings_by_severity: SeverityCounts {
                critical: 1,
                high: 2,
                medium: 3,
                low: 4,
                info: 5,
            },
            suppressed_by_rule: suppressed.clone(),
            tokens_in: 12_345,
            tokens_out: 678,
            tokens_cache_read: 8_000,
            duration_ms: 4_200,
            flags: Flags {
                deep: true,
                parallel_n: 4,
                ensemble: true,
            },
            mode: Some("plan".to_string()),
            context: populated_context_telemetry(),
            finding_ids: vec!["fid-AAA".into(), "fid-BBB".into()],
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        };

        log.record(&rec).unwrap();
        let loaded = log.load_all().unwrap();

        assert_eq!(loaded.len(), 1);
        let got = &loaded[0];

        assert_eq!(got.run_id, rec.run_id);
        assert_eq!(got.timestamp, rec.timestamp);
        assert_eq!(got.quorum_version, rec.quorum_version);
        assert_eq!(got.repo, rec.repo);
        assert_eq!(got.invoked_from, rec.invoked_from);
        assert_eq!(got.model, rec.model);
        assert_eq!(got.files_reviewed, rec.files_reviewed);
        assert_eq!(got.lines_added, rec.lines_added);
        assert_eq!(got.lines_removed, rec.lines_removed);
        assert_eq!(got.findings_by_severity, rec.findings_by_severity);
        assert_eq!(got.suppressed_by_rule, rec.suppressed_by_rule);
        assert_eq!(got.tokens_in, rec.tokens_in);
        assert_eq!(got.tokens_out, rec.tokens_out);
        assert_eq!(got.tokens_cache_read, rec.tokens_cache_read);
        assert_eq!(got.duration_ms, rec.duration_ms);
        assert_eq!(got.flags, rec.flags);
        assert_eq!(got.mode, rec.mode);
        assert_eq!(got.context, rec.context);
        assert_eq!(got.finding_ids, rec.finding_ids);
    }

    #[test]
    fn sqlite_optional_fields_round_trip() {
        // None/empty values round-trip correctly: repo=None,
        // lines_added/removed=None, mode=None, empty suppressed_by_rule,
        // empty finding_ids, default context.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);

        let rec = test_review_record("01TEST_OPTIONAL_FIELDS001");

        log.record(&rec).unwrap();
        let loaded = log.load_all().unwrap();

        assert_eq!(loaded.len(), 1);
        let got = &loaded[0];

        assert!(got.repo.is_none(), "repo should be None");
        assert!(got.lines_added.is_none(), "lines_added should be None");
        assert!(got.lines_removed.is_none(), "lines_removed should be None");
        assert!(got.mode.is_none(), "mode should be None");
        assert!(
            got.suppressed_by_rule.is_empty(),
            "suppressed_by_rule should be empty"
        );
        assert!(got.finding_ids.is_empty(), "finding_ids should be empty");
        assert_eq!(got.context, ContextTelemetry::default());
        assert_eq!(got.flags, Flags::default());
    }

    #[test]
    fn sqlite_context_telemetry_round_trip() {
        // Set 10+ ContextTelemetry fields including nested LegCounts;
        // verify they survive the JSON column round-trip.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);

        let ctx = ContextTelemetry {
            auto_inject_enabled: true,
            injector_available: true,
            retriever_errored: false,
            retrieved_chunk_count: 12,
            injected_chunk_count: 5,
            injected_tokens: 320,
            below_threshold_count: 7,
            adaptive_threshold_applied: true,
            effective_prose_threshold: 0.72,
            injected_chunk_ids: vec!["c1".into(), "c2".into(), "c3".into()],
            injected_sources: vec!["src-alpha".into(), "src-beta".into()],
            precedence_entries: 3,
            render_duration_ms: 88,
            rendered_prompt_hash: Some("abcdef0123456789".into()),
            suppressed_by_calibrator: 2,
            suppressed_by_floor: 1,
            nan_scores_dropped: 0,
            retrieved_by_leg: LegCounts {
                bm25: 4,
                vector: 5,
                structural: 3,
                structural_only: 1,
                total_unique: 10,
            },
            injected_by_leg: LegCounts {
                bm25: 2,
                vector: 2,
                structural: 1,
                structural_only: 0,
                total_unique: 5,
            },
            rerank_score_min: Some(0.32),
            rerank_score_p10: Some(0.45),
            rerank_score_median: Some(0.68),
            rerank_score_p90: Some(0.91),
            sources_queried: 2,
            sources_contributing: 1,
            per_source_contributions: std::collections::BTreeMap::new(),
            dep_boost_applied: 0,
            current_repo_reserved_available: 3,
            current_repo_reserved_filled: 2,
            structural_fingerprints_computed: 0,
            structural_knn_queries: 0,
            structural_knn_hits: 0,
            structural_boost_applied: 0,
        };

        let mut rec = test_review_record("01TEST_CTX_TELEMETRY0001");
        rec.context = ctx.clone();

        log.record(&rec).unwrap();
        let loaded = log.load_all().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].context, ctx);

        // Spot-check nested LegCounts fields survived.
        assert_eq!(loaded[0].context.retrieved_by_leg.bm25, 4);
        assert_eq!(loaded[0].context.retrieved_by_leg.structural_only, 1);
        assert_eq!(loaded[0].context.injected_by_leg.total_unique, 5);
        assert_eq!(loaded[0].context.rerank_score_median, Some(0.68));
    }

    #[test]
    fn sqlite_finding_ids_normalized() {
        // Verify finding_ids go to the child table and join back
        // correctly. Also verify multiple records with different
        // finding_ids stay isolated.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);

        let mut rec1 = test_review_record("01TEST_FINDIDS_A00000001");
        rec1.finding_ids = vec!["fid-1".into(), "fid-2".into(), "fid-3".into()];

        let mut rec2 = test_review_record("01TEST_FINDIDS_B00000001");
        rec2.finding_ids = vec!["fid-X".into()];

        let rec3 = test_review_record("01TEST_FINDIDS_C00000001");
        // rec3 has no finding_ids

        log.record(&rec1).unwrap();
        log.record(&rec2).unwrap();
        log.record(&rec3).unwrap();

        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 3);

        // Find each by run_id.
        let got1 = loaded.iter().find(|r| r.run_id == rec1.run_id).unwrap();
        let got2 = loaded.iter().find(|r| r.run_id == rec2.run_id).unwrap();
        let got3 = loaded.iter().find(|r| r.run_id == rec3.run_id).unwrap();

        assert_eq!(got1.finding_ids, vec!["fid-1", "fid-2", "fid-3"]);
        assert_eq!(got2.finding_ids, vec!["fid-X"]);
        assert!(got3.finding_ids.is_empty());
    }

    #[test]
    fn sqlite_load_all_ordered_by_timestamp() {
        // Multiple records with different timestamps; verify load_all
        // returns them in ascending timestamp order regardless of
        // insertion order.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);

        let ts3 = chrono::DateTime::parse_from_rfc3339("2026-05-12T03:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts1 = chrono::DateTime::parse_from_rfc3339("2026-05-12T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ts2 = chrono::DateTime::parse_from_rfc3339("2026-05-12T02:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        // Insert out of order: 3, 1, 2
        let mut r3 = test_review_record("01TEST_ORDER_C000000001");
        r3.timestamp = ts3;
        let mut r1 = test_review_record("01TEST_ORDER_A000000001");
        r1.timestamp = ts1;
        let mut r2 = test_review_record("01TEST_ORDER_B000000001");
        r2.timestamp = ts2;

        log.record(&r3).unwrap();
        log.record(&r1).unwrap();
        log.record(&r2).unwrap();

        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].run_id, r1.run_id, "earliest first");
        assert_eq!(loaded[1].run_id, r2.run_id, "middle second");
        assert_eq!(loaded[2].run_id, r3.run_id, "latest last");
    }

    #[test]
    fn sqlite_path_returns_empty() {
        // The SQLite backend's path() should return an empty Path.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        assert_eq!(log.path(), Path::new(""));
    }

    #[test]
    fn sqlite_load_all_empty_db() {
        // load_all on an empty SQLite database should return an empty Vec.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let loaded = log.load_all().unwrap();
        assert!(loaded.is_empty());
    }

    // ── load_recent tests ────────────────────────────────────────────

    /// Minimal fixture for `load_recent` tests. Callers override fields
    /// as needed (e.g. `timestamp`, `files_reviewed`).
    fn test_record() -> ReviewRecord {
        ReviewRecord {
            run_id: ReviewRecord::new_ulid(),
            timestamp: Utc::now(),
            quorum_version: "test".into(),
            repo: Some("test-repo".into()),
            invoked_from: "test".into(),
            model: "test-model".into(),
            files_reviewed: 1,
            lines_added: None,
            lines_removed: None,
            findings_by_severity: SeverityCounts::default(),
            suppressed_by_rule: HashMap::new(),
            tokens_in: 100,
            tokens_out: 50,
            tokens_cache_read: 0,
            duration_ms: 500,
            flags: Flags::default(),
            mode: None,
            context: ContextTelemetry::default(),
            finding_ids: vec![],
            skills_used: Vec::new(),
            skill_findings: None,
            integrator_findings_out: None,
        }
    }

    #[test]
    fn load_recent_returns_last_n_records_in_chronological_order() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        for i in 0..10u32 {
            let mut rec = test_record();
            rec.timestamp = chrono::Utc::now() + chrono::Duration::seconds(i64::from(i));
            rec.files_reviewed = i;
            log.record(&rec).unwrap();
        }

        let recent = log.load_recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        // Chronological order: oldest-of-the-3 first.
        assert_eq!(recent[0].files_reviewed, 7);
        assert_eq!(recent[1].files_reviewed, 8);
        assert_eq!(recent[2].files_reviewed, 9);
    }

    #[test]
    fn load_recent_returns_all_when_n_exceeds_total() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        for _ in 0..3 {
            log.record(&test_record()).unwrap();
        }

        let recent = log.load_recent(100).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn load_recent_zero_returns_empty() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);
        log.record(&test_record()).unwrap();

        let recent = log.load_recent(0).unwrap();
        assert!(recent.is_empty());
    }

    // ── load_since tests ────────────────────────────────────────────

    #[test]
    fn load_since_returns_only_records_after_cutoff() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        let t1 = chrono::Utc::now() - chrono::Duration::days(10);
        let t2 = chrono::Utc::now() - chrono::Duration::days(5);
        let t3 = chrono::Utc::now();
        let cutoff = chrono::Utc::now() - chrono::Duration::days(7);

        let mut r1 = test_record();
        r1.timestamp = t1;
        r1.files_reviewed = 1;
        let mut r2 = test_record();
        r2.timestamp = t2;
        r2.files_reviewed = 2;
        let mut r3 = test_record();
        r3.timestamp = t3;
        r3.files_reviewed = 3;

        log.record(&r1).unwrap();
        log.record(&r2).unwrap();
        log.record(&r3).unwrap();

        let since = log.load_since(cutoff).unwrap();
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].files_reviewed, 2);
        assert_eq!(since[1].files_reviewed, 3);
    }

    #[test]
    fn load_since_returns_empty_when_all_older() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        let mut rec = test_record();
        rec.timestamp = chrono::Utc::now() - chrono::Duration::days(30);
        log.record(&rec).unwrap();

        let since = log.load_since(chrono::Utc::now()).unwrap();
        assert!(since.is_empty());
    }

    // ── load_all_finding_ids tests ──────────────────────────────────────

    #[test]
    fn load_all_finding_ids_returns_unique_ids() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        let mut r1 = test_record();
        r1.finding_ids = vec!["fid-1".into(), "fid-2".into()];
        let mut r2 = test_record();
        r2.finding_ids = vec!["fid-2".into(), "fid-3".into()];

        log.record(&r1).unwrap();
        log.record(&r2).unwrap();

        let ids = log.load_all_finding_ids().unwrap();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains("fid-1"));
        assert!(ids.contains("fid-2"));
        assert!(ids.contains("fid-3"));
    }

    #[test]
    fn load_all_finding_ids_empty_when_no_records() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        let ids = log.load_all_finding_ids().unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn load_all_finding_ids_empty_when_records_have_no_findings() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        log.record(&test_record()).unwrap();

        let ids = log.load_all_finding_ids().unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn count_returns_total_records() {
        let handle = crate::storage::in_memory_handle();
        let log = ReviewLog::with_storage(handle);

        assert_eq!(log.count().unwrap(), 0);

        log.record(&test_record()).unwrap();
        assert_eq!(log.count().unwrap(), 1);

        log.record(&test_record()).unwrap();
        assert_eq!(log.count().unwrap(), 2);
    }

    // -- Per-skill identity fields (#405) --

    #[test]
    fn legacy_review_record_deserializes_with_empty_skills() {
        let legacy = r#"{"run_id":"01ABC","timestamp":"2026-01-01T00:00:00Z","quorum_version":"0.1","repo":null,"invoked_from":"tty","model":"gpt","files_reviewed":1,"lines_added":null,"lines_removed":null,"findings_by_severity":{"critical":0,"high":0,"medium":0,"low":0,"info":0},"tokens_in":0,"tokens_out":0,"duration_ms":0}"#;
        let rec: ReviewRecord = serde_json::from_str(legacy).expect("legacy load");
        assert!(rec.skills_used.is_empty());
        assert_eq!(rec.skill_findings, None);
        assert_eq!(rec.integrator_findings_out, None);
    }

    #[test]
    fn record_with_skills_used_roundtrips() {
        let mut rec = sample_record();
        rec.skills_used = vec!["security-reviewer".into(), "perf-analyzer".into()];
        rec.skill_findings = Some({
            let mut m = HashMap::new();
            m.insert("security-reviewer".into(), 3);
            m.insert("perf-analyzer".into(), 1);
            m
        });
        rec.integrator_findings_out = Some(4);

        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"skills_used\""));
        assert!(json.contains("\"skill_findings\""));
        assert!(json.contains("\"integrator_findings_out\":4"));

        let back: ReviewRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.skills_used, rec.skills_used);
        assert_eq!(back.skill_findings, rec.skill_findings);
        assert_eq!(back.integrator_findings_out, rec.integrator_findings_out);
    }

    #[test]
    fn record_omits_skills_keys_when_empty() {
        let rec = sample_record();
        assert!(rec.skills_used.is_empty());
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("skills_used"),
            "empty skills_used must not write the key: {json}"
        );
        assert!(
            !json.contains("skill_findings"),
            "None skill_findings must not write the key: {json}"
        );
        assert!(
            !json.contains("integrator_findings_out"),
            "None integrator_findings_out must not write the key: {json}"
        );
    }

    // ── FindingMeta / record_with_meta tests ───────────────────────────

    #[test]
    fn sqlite_finding_meta_round_trip() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = test_review_record("01TEST_META_ROUNDTRIP001");
        record.finding_ids = vec!["F1".into(), "F2".into()];
        let meta = vec![
            FindingMeta {
                id: "F1".into(),
                title: "SQL injection".into(),
                file_path: "src/auth.rs".into(),
            },
            FindingMeta {
                id: "F2".into(),
                title: "XSS risk".into(),
                file_path: "src/web.rs".into(),
            },
        ];
        log.record_with_meta(&record, &meta).unwrap();

        let conn = match &log.backend {
            Backend::Sqlite(h) => h.lock().unwrap(),
            _ => panic!("expected sqlite"),
        };
        let mut stmt = conn
            .prepare(
                "SELECT finding_id, title, file_path FROM review_finding_ids WHERE run_id = ?1 ORDER BY rowid",
            )
            .unwrap();
        let rows: Vec<(String, String, String)> = stmt
            .query_map(rusqlite::params![record.run_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            ("F1".into(), "SQL injection".into(), "src/auth.rs".into())
        );
        assert_eq!(
            rows[1],
            ("F2".into(), "XSS risk".into(), "src/web.rs".into())
        );
    }

    #[test]
    fn sqlite_finding_meta_empty_vec_writes_no_child_rows() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let record = test_review_record("01TEST_META_EMPTY0000001");
        log.record_with_meta(&record, &[]).unwrap();

        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].finding_ids.is_empty());
    }

    #[test]
    fn sqlite_finding_meta_review_record_still_loads_via_load_all() {
        // Verify the reviews INSERT is compatible with load_all's JOIN
        // path even when we write via record_with_meta.
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = test_review_record("01TEST_META_LOADALL0001");
        record.finding_ids = vec!["FA".into()];
        let meta = vec![FindingMeta {
            id: "FA".into(),
            title: "Buffer overflow".into(),
            file_path: "src/lib.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();

        let loaded = log.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].finding_ids, vec!["FA"]);
    }

    // ── resolve_finding_id tests ─────────────────────────────────────

    #[test]
    fn resolve_finding_id_exact_match() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection risk".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("src/auth.rs", "SQL injection risk");
        assert_eq!(result, Some("F1".to_string()));
    }

    #[test]
    fn resolve_finding_id_partial_match() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection vulnerability in auth module".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("src/auth.rs", "SQL injection");
        assert_eq!(result, Some("F1".to_string()));
    }

    #[test]
    fn resolve_finding_id_no_match_wrong_file() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("src/other.rs", "SQL injection");
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_finding_id_no_match_below_threshold() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection vulnerability".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result =
            log.resolve_finding_id("src/auth.rs", "completely unrelated finding title xyz");
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_finding_id_skips_legacy_empty_title() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["LEGACY".into()];
        log.record(&record).unwrap();
        let result = log.resolve_finding_id("src/auth.rs", "SQL injection");
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_finding_id_matches_despite_backticks() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "`predict_one` trusts inconsistent public `LogisticFit` field lengths".into(),
            file_path: "src/logistic.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id(
            "src/logistic.rs",
            "predict_one trusts inconsistent public LogisticFit field lengths",
        );
        assert_eq!(result, Some("F1".to_string()));
    }

    #[test]
    fn normalize_title_strips_markdown() {
        assert_eq!(
            ReviewLog::normalize_title("`predict_one` is **bad**"),
            "predict one is bad"
        );
        assert_eq!(
            ReviewLog::normalize_title("no_formatting_here"),
            "no formatting here"
        );
        assert_eq!(ReviewLog::normalize_title(""), "");
        assert_eq!(ReviewLog::normalize_title("plain text"), "plain text");
    }

    #[test]
    fn resolve_finding_id_short_query_via_containment() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection vulnerability in authentication module via unsanitized input"
                .into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("src/auth.rs", "SQL injection");
        assert_eq!(result, Some("F1".to_string()));
    }

    #[test]
    fn resolve_finding_id_path_normalization() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "SQL injection risk".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("./src/auth.rs", "SQL injection risk");
        assert_eq!(result, Some("F1".to_string()));
    }

    #[test]
    fn resolve_finding_id_rejects_unrelated_same_file() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "Function main has cyclomatic complexity 60".into(),
            file_path: "src/main.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id(
            "src/main.rs",
            "error-reporting path panics because it calls unwrap()",
        );
        assert_eq!(result, None);
    }

    #[test]
    fn resolve_finding_id_stop_words_dont_inflate_score() {
        let dir = TempDir::new().unwrap();
        let log = sqlite_review_log(&dir);
        let mut record = sample_record();
        record.finding_ids = vec!["F1".into()];
        let meta = vec![FindingMeta {
            id: "F1".into(),
            title: "The missing validation of the input is a risk to the system".into(),
            file_path: "src/auth.rs".into(),
        }];
        log.record_with_meta(&record, &meta).unwrap();
        let result = log.resolve_finding_id("src/auth.rs", "missing input validation");
        assert_eq!(result, Some("F1".to_string()));
    }
}
