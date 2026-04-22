//! Feedback index for semantic precedent retrieval.
//! When embeddings feature is enabled, uses vector similarity.
//! Falls back to word Jaccard when embeddings not available.

use crate::feedback::{FeedbackEntry, FeedbackStore};
#[cfg(feature = "embeddings")]
use crate::patterns;

pub struct SimilarEntry {
    pub entry: FeedbackEntry,
    pub similarity: f32,
}

pub struct FeedbackIndex {
    entries: Vec<FeedbackEntry>,
    #[cfg(feature = "embeddings")]
    vectors: Vec<Vec<f32>>,
    #[cfg(feature = "embeddings")]
    embedder: Option<crate::embeddings::LocalEmbedder>,
    /// Optional BM25 keyword index (used by `--fast` mode). When present,
    /// it takes precedence over both embeddings and Jaccard.
    bm25_engine: Option<bm25::SearchEngine<u32>>,
}

/// Produce the indexable document text for a feedback entry.
///
/// Joins title + reason so BM25 and embeddings can disambiguate entries whose
/// titles alone share generic programming vocabulary (e.g. two "Missing input
/// validation" entries where one reason mentions JWT and the other mentions
/// API body types). Empty reasons degrade to title-only so legacy entries
/// without a reason are unaffected. See docs/ notes from Gemini/GPT review
/// (2026-04-20) on why corpus enrichment beats model upgrade for our scale.
fn bm25_doc_text(entry: &FeedbackEntry) -> String {
    let reason = entry.reason.trim();
    if reason.is_empty() {
        entry.finding_title.clone()
    } else {
        format!("{} {}", entry.finding_title, reason)
    }
}

impl FeedbackIndex {
    /// Build a Jaccard-only index without initializing fastembed.
    /// Used as a last-resort fallback when both embeddings and BM25 are unavailable.
    pub fn build_jaccard_only(store: &FeedbackStore) -> anyhow::Result<Self> {
        let entries = store.load_all()?;
        Ok(Self {
            entries,
            #[cfg(feature = "embeddings")]
            vectors: vec![],
            #[cfg(feature = "embeddings")]
            embedder: None,
            bm25_engine: None,
        })
    }

    /// Build a hybrid index combining BM25 + fastembed. At query time both
    /// retrievers run and their ranked lists are fused with Reciprocal Rank
    /// Fusion (k=60). Uses the most memory of all modes (BM25 index + embed
    /// model + vectors) but produces the highest retrieval quality.
    #[cfg(feature = "embeddings")]
    pub fn build_hybrid(store: &FeedbackStore) -> anyhow::Result<Self> {
        let entries = store.load_all()?;
        let bm25_engine = if entries.is_empty() {
            None
        } else {
            let corpus: Vec<String> = entries.iter().map(bm25_doc_text).collect();
            Some(
                bm25::SearchEngineBuilder::<u32>::with_corpus(bm25::Language::English, corpus)
                    .build(),
            )
        };
        match crate::embeddings::LocalEmbedder::new() {
            Ok(mut embedder) => {
                let texts: Vec<String> = entries
                    .iter()
                    .map(|e| {
                        let pattern = patterns::classify_pattern(
                            &e.finding_title,
                            "",
                            &e.finding_category,
                        );
                        patterns::embedding_text_enriched(
                            &e.finding_title,
                            &e.finding_category,
                            pattern.as_deref(),
                            &[&e.reason],
                        )
                    })
                    .collect();
                let vectors = if texts.is_empty() {
                    vec![]
                } else {
                    embedder.embed_batch(&texts)?
                };
                // Retrieval indexes by entry position into self.vectors, so an
                // off-by-one between texts and vectors would silently attach
                // similarity scores to the wrong FeedbackEntry.
                anyhow::ensure!(
                    vectors.len() == entries.len(),
                    "embedding alignment mismatch: {} entries vs {} vectors",
                    entries.len(),
                    vectors.len(),
                );
                Ok(Self {
                    entries,
                    vectors,
                    embedder: Some(embedder),
                    bm25_engine,
                })
            }
            Err(_) => {
                // Fall back to BM25-only if fastembed unavailable.
                Ok(Self {
                    entries,
                    vectors: vec![],
                    embedder: None,
                    bm25_engine,
                })
            }
        }
    }

    /// Build a BM25-only index. Used by `--fast` mode: skips the ~1.5 GB
    /// fastembed model and ~15 s startup, while still handling rare-term
    /// weighting better than plain Jaccard.
    pub fn build_bm25(store: &FeedbackStore) -> anyhow::Result<Self> {
        let entries = store.load_all()?;
        let bm25_engine = if entries.is_empty() {
            None
        } else {
            let corpus: Vec<String> = entries.iter().map(bm25_doc_text).collect();
            Some(
                bm25::SearchEngineBuilder::<u32>::with_corpus(bm25::Language::English, corpus)
                    .build(),
            )
        };
        Ok(Self {
            entries,
            #[cfg(feature = "embeddings")]
            vectors: vec![],
            #[cfg(feature = "embeddings")]
            embedder: None,
            bm25_engine,
        })
    }

    pub fn build(store: &FeedbackStore) -> anyhow::Result<Self> {
        let entries = store.load_all()?;

        // BM25 is always built alongside (unless disabled or store is empty)
        // so that RRF fusion works even when embeddings fail to initialize —
        // and so callers without embeddings fall back to BM25, not Jaccard.
        // QUORUM_NO_RRF=1 disables the BM25 side for A/B comparison.
        let rrf_disabled = std::env::var("QUORUM_NO_RRF").ok().as_deref() == Some("1");
        let bm25_engine = if entries.is_empty() || rrf_disabled {
            None
        } else {
            let corpus: Vec<String> = entries.iter().map(bm25_doc_text).collect();
            Some(
                bm25::SearchEngineBuilder::<u32>::with_corpus(bm25::Language::English, corpus)
                    .build(),
            )
        };

        #[cfg(feature = "embeddings")]
        {
            match crate::embeddings::LocalEmbedder::new() {
                Ok(mut embedder) => {
                    let texts: Vec<String> = entries.iter()
                        .map(|e| {
                            let pattern = patterns::classify_pattern(&e.finding_title, "", &e.finding_category);
                            patterns::embedding_text_enriched(
                                &e.finding_title,
                                &e.finding_category,
                                pattern.as_deref(),
                                &[&e.reason],
                            )
                        })
                        .collect();
                    let vectors = if texts.is_empty() {
                        vec![]
                    } else {
                        embedder.embed_batch(&texts)?
                    };
                    anyhow::ensure!(
                        vectors.len() == entries.len(),
                        "embedding alignment mismatch: {} entries vs {} vectors",
                        entries.len(),
                        vectors.len(),
                    );
                    tracing::debug!(entries = entries.len(), "FeedbackIndex: embedded with bge-small-en-v1.5");
                    return Ok(Self { entries, vectors, embedder: Some(embedder), bm25_engine });
                }
                Err(e) => {
                    eprintln!("FeedbackIndex: embedding model unavailable ({}), falling back to BM25+Jaccard", e);
                }
            }
        }

        Ok(Self {
            entries,
            #[cfg(feature = "embeddings")]
            vectors: vec![],
            #[cfg(feature = "embeddings")]
            embedder: None,
            bm25_engine,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Like `find_similar` but appends free-text discriminators to the query so
    /// paraphrased findings can be disambiguated by concrete tokens (function
    /// signatures, framework names, sink keywords, etc.). Empty/whitespace-only
    /// discriminators are filtered to match corpus-side behavior.
    pub fn find_similar_enriched(
        &mut self,
        finding_title: &str,
        category: &str,
        discriminators: &[&str],
        top_k: usize,
    ) -> Vec<SimilarEntry> {
        // Build an enriched query once. Downstream methods that need plain title
        // (e.g. BM25 tokenization) accept the whole string; BM25 tokenizes and
        // IDF self-weights so the extra tokens don't hurt rare-title matches.
        let extras: Vec<&str> = discriminators.iter()
            .copied()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if extras.is_empty() {
            return self.find_similar(finding_title, category, top_k);
        }
        let enriched_query = format!("{} {}", finding_title, extras.join(" "));
        self.find_similar(&enriched_query, category, top_k)
    }

    pub fn find_similar(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        // Hybrid path: both BM25 and embeddings present → RRF fuse.
        #[cfg(feature = "embeddings")]
        if self.bm25_engine.is_some()
            && self.embedder.is_some()
            && !self.vectors.is_empty()
        {
            return self.find_similar_hybrid_rrf(finding_title, category, top_k);
        }

        if self.bm25_engine.is_some() {
            return self.find_similar_bm25(finding_title, category, top_k);
        }

        #[cfg(feature = "embeddings")]
        if self.embedder.is_some() && !self.vectors.is_empty() {
            return self.find_similar_embedding(finding_title, category, top_k);
        }

        self.find_similar_jaccard(finding_title, category, top_k)
    }

    /// Reciprocal Rank Fusion over BM25 + embedding result lists.
    /// Standard k=60. Pulls a deeper candidate pool per method (3×top_k) so
    /// the tail of each retriever gets a chance to surface items the other
    /// missed.
    #[cfg(feature = "embeddings")]
    fn find_similar_hybrid_rrf(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        const K: f32 = 60.0;
        let pool = (top_k * 3).max(20);

        // BM25 ranked list → (entry_index, rank)
        let bm25_results = if let Some(engine) = &self.bm25_engine {
            engine.search(finding_title, pool)
        } else {
            Vec::new()
        };
        let bm25_ranked: Vec<(u32, usize)> = bm25_results
            .iter()
            .enumerate()
            .map(|(rank, r)| (r.document.id, rank))
            .collect();

        // Embedding ranked list → (entry_index, rank). Use index-preserving
        // helper so we never misattribute ranks when two entries share the
        // same (title, category, timestamp).
        let embed_ranked: Vec<(u32, usize)> = self
            .embed_rank_indices(finding_title, category, pool)
            .into_iter()
            .enumerate()
            .map(|(rank, (idx, _sim))| (idx as u32, rank))
            .collect();

        // Accumulate RRF scores keyed by entry index.
        let mut scores: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
        for (id, rank) in bm25_ranked.iter().chain(embed_ranked.iter()) {
            *scores.entry(*id).or_insert(0.0) += 1.0 / (K + *rank as f32 + 1.0);
        }

        let mut fused: Vec<(u32, f32)> = scores.into_iter().collect();
        fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Normalize fused scores to [0, 1] for calibrator threshold compatibility.
        let max = fused.first().map(|(_, s)| *s).unwrap_or(1e-6).max(1e-6);
        fused
            .into_iter()
            .take(top_k)
            .filter_map(|(id, s)| {
                self.entries.get(id as usize).map(|e| SimilarEntry {
                    entry: e.clone(),
                    similarity: (s / max).clamp(0.0, 1.0),
                })
            })
            .collect()
    }

    fn find_similar_bm25(&self, finding_title: &str, _category: &str, top_k: usize) -> Vec<SimilarEntry> {
        let engine = match &self.bm25_engine {
            Some(e) => e,
            None => return Vec::new(),
        };
        // Fetch a few more than top_k so normalization is stable.
        let results = engine.search(finding_title, top_k.max(1));
        if results.is_empty() {
            return Vec::new();
        }
        let max_score = results
            .iter()
            .map(|r| r.score)
            .fold(f32::MIN, f32::max)
            .max(1e-6);
        results
            .into_iter()
            .filter_map(|r| {
                self.entries
                    .get(r.document.id as usize)
                    .map(|e| SimilarEntry {
                        entry: e.clone(),
                        similarity: (r.score / max_score).clamp(0.0, 1.0),
                    })
            })
            .collect()
    }

    fn find_similar_jaccard(&self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        self.jaccard_rank_indices(finding_title, category, top_k)
            .into_iter()
            .filter_map(|(idx, sim)| {
                self.entries.get(idx).map(|e| SimilarEntry { entry: e.clone(), similarity: sim })
            })
            .collect()
    }

    /// Rank entries by Jaccard + category match, returning (entry_index, sim).
    /// Index-preserving (mirrors `embed_rank_indices`) so duplicate-title
    /// entries stay distinct during RRF fusion.
    fn jaccard_rank_indices(&self, finding_title: &str, category: &str, top_k: usize) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = self.entries.iter().enumerate()
            .map(|(i, e)| {
                let title_sim = word_jaccard(finding_title, &e.finding_title);
                let cat_match = if !e.finding_category.is_empty() && category == e.finding_category { 0.4 } else { 0.0 };
                (i, (title_sim * 0.6 + cat_match) as f32)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }

    #[cfg(feature = "embeddings")]
    fn find_similar_embedding(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<SimilarEntry> {
        self.embed_rank_indices(finding_title, category, top_k)
            .into_iter()
            .filter_map(|(idx, sim)| {
                self.entries.get(idx).map(|e| SimilarEntry { entry: e.clone(), similarity: sim })
            })
            .collect()
    }

    /// Rank entries by embedding cosine similarity, returning (entry_index, sim)
    /// pairs. Preserving indices (rather than looking them up by field match
    /// later) is load-bearing for RRF: duplicate (title, category, timestamp)
    /// entries would otherwise collapse to the first match.
    #[cfg(feature = "embeddings")]
    fn embed_rank_indices(&mut self, finding_title: &str, category: &str, top_k: usize) -> Vec<(usize, f32)> {
        let pattern = patterns::classify_pattern(finding_title, "", category);
        let query_text = patterns::embedding_text(finding_title, category, pattern.as_deref());

        let embedder = self
            .embedder
            .as_mut()
            .expect("embed_rank_indices requires an initialized embedder");
        let query_vec = match embedder.embed(&query_text) {
            Ok(v) => v,
            Err(_) => {
                // Jaccard fallback preserves indices directly — no .position() lookup.
                return self.jaccard_rank_indices(finding_title, category, top_k);
            }
        };

        let mut scored: Vec<(usize, f32)> = self
            .vectors
            .iter()
            .enumerate()
            .map(|(i, vec)| (i, crate::embeddings::cosine_similarity(&query_vec, vec)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);
        scored
    }
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() { return 0.0; }
    // Lowercase so equivalent titles like "SQL Injection" / "sql injection"
    // don't split into disjoint token sets.
    let al = a.to_lowercase();
    let bl = b.to_lowercase();
    let wa: std::collections::HashSet<&str> = al.split_whitespace().collect();
    let wb: std::collections::HashSet<&str> = bl.split_whitespace().collect();
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FeedbackStore, Verdict, Provenance};
    use tempfile::TempDir;
    use chrono::Utc;

    fn make_entry(title: &str, category: &str, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: title.into(),
            finding_category: category.into(),
            verdict,
            reason: "test".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: Provenance::Unknown,
        }
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn default_build_uses_rrf_when_embeddings_available() {
        // Default build() should construct both retrievers so find_similar
        // dispatches to the RRF hybrid path. This is the quality improvement
        // shipped as the default.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        store.record(&make_entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
        let index = FeedbackIndex::build(&store).unwrap();
        assert!(
            index.embedder.is_some(),
            "default build must load embeddings"
        );
        assert!(
            index.bm25_engine.is_some(),
            "default build must also build BM25 for RRF fusion"
        );
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn build_hybrid_rrf_returns_fused_ranking() {
        // Hybrid path runs both BM25 and embeddings, fuses via RRF.
        // Must return results (non-empty) and the top hit must be a
        // plausible BM25 or embedding match, not noise.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        for (t, c) in &[
            ("SQL injection via string concat", "security"),
            ("SQL injection in query builder", "security"),
            ("Unused imports increase noise", "style"),
            ("Hardcoded API key in source", "security"),
        ] {
            store.record(&make_entry(t, c, Verdict::Tp)).unwrap();
        }
        let mut index = FeedbackIndex::build_hybrid(&store).unwrap();
        let similar = index.find_similar("SQL injection risk", "security", 3);
        assert!(!similar.is_empty());
        // First result should be SQL-related
        assert!(
            similar[0].entry.finding_title.contains("SQL"),
            "RRF top hit should be SQL-related, got: {}",
            similar[0].entry.finding_title
        );
        // Similarity is normalized 0-1
        assert!(similar[0].similarity > 0.0 && similar[0].similarity <= 1.0);
    }

    #[test]
    fn build_bm25_returns_rare_token_match() {
        // BM25 should weight rare terms more heavily than shared boilerplate.
        // Query for "SQL injection" should retrieve SQL-injection entries
        // ahead of generic "Unused import" or "complexity" entries.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        for (t, c) in &[
            ("SQL injection in user input auth path", "security"),
            ("Unused import os.path module", "style"),
            ("Function foo has cyclomatic complexity 12", "complexity"),
            ("SQL injection via f-string formatting", "security"),
            ("Function bar has cyclomatic complexity 8", "complexity"),
        ] {
            store.record(&make_entry(t, c, Verdict::Tp)).unwrap();
        }
        let mut index = FeedbackIndex::build_bm25(&store).unwrap();
        let similar = index.find_similar("SQL injection risk in query builder", "security", 3);
        assert!(similar.len() >= 2);
        // Top-2 results must be the SQL entries, not complexity/style
        assert!(
            similar[0].entry.finding_title.contains("SQL"),
            "top result should be SQL-related, got: {}",
            similar[0].entry.finding_title
        );
        assert!(similar[1].entry.finding_title.contains("SQL"));
    }

    #[test]
    fn bm25_corpus_includes_reason_for_discriminator_retrieval() {
        // Two entries with IDENTICAL titles/categories but different reasons.
        // A query containing tokens from Entry B's reason should rank Entry B
        // higher — even though the title alone is ambiguous — because the
        // corpus is indexed with the reason text, not just the title. This
        // is the enrichment Gemini 3 Pro + GPT-5.2 recommended for fixing
        // the "Missing input validation" / "Missing JWT validation" conflation.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));

        // Insert entry A FIRST so if the corpus is title-only, identical BM25 scores
        // tie and insertion order wins — which would put A ahead of B. The test then
        // only passes if B is actively elevated by reason-matching.
        let mut e_a = make_entry("Missing input validation", "security", Verdict::Fp);
        e_a.reason = "API endpoint does not check request body types".into();
        let mut e_b = make_entry("Missing input validation", "security", Verdict::Fp);
        e_b.reason = "JWT verification uses algorithm=none allowing signature bypass".into();
        store.record(&e_a).unwrap();
        store.record(&e_b).unwrap();

        let mut index = FeedbackIndex::build_bm25(&store).unwrap();
        let similar = index.find_similar(
            "Missing input validation jwt signature algorithm",
            "security",
            2,
        );
        assert_eq!(similar.len(), 2);
        assert_eq!(similar[0].entry.reason, e_b.reason,
            "JWT-reason entry must rank ahead despite being inserted second; \
             if you see the API-body reason here, the corpus is not enriched with reason text. \
             got top reason: {:?}", similar[0].entry.reason);
        // And B's similarity should exceed A's, not tie.
        assert!(similar[0].similarity > similar[1].similarity,
            "top sim {} must exceed second {}, else the ranking was an insertion-order tie",
            similar[0].similarity, similar[1].similarity);
    }

    #[test]
    fn find_similar_enriched_uses_discriminators_without_them_in_title() {
        // Entry has a generic title but a discriminative reason. A caller with
        // hydration context (JWT/signature tokens from the finding's description
        // and evidence) should reach this entry via discriminators, NOT by having
        // to cram those tokens into the raw title. This is how calibrator.rs /
        // pipeline.rs will eventually pass hydration context through.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));

        let mut e_api = make_entry("Missing input validation", "security", Verdict::Fp);
        e_api.reason = "request body parameters not validated before DB write".into();
        let mut e_jwt = make_entry("Missing input validation", "security", Verdict::Fp);
        e_jwt.reason = "JWT verify uses algorithm none allowing signature bypass".into();
        store.record(&e_api).unwrap();
        store.record(&e_jwt).unwrap();

        let mut index = FeedbackIndex::build_bm25(&store).unwrap();

        // JWT-flavored discriminators must rank the JWT-reason entry first.
        let with_jwt = index.find_similar_enriched(
            "Missing input validation",
            "security",
            &["jwt", "signature", "algorithm"],
            2,
        );
        assert_eq!(with_jwt.len(), 2);
        assert_eq!(with_jwt[0].entry.reason, e_jwt.reason,
            "JWT discriminators must rank JWT-reason first; got: {:?}",
            with_jwt[0].entry.reason);

        // API-flavored discriminators must flip the ranking — API-reason first.
        // Proves the discriminators are actually steering retrieval, not just
        // matching the lexically-richer JWT reason always.
        let with_api = index.find_similar_enriched(
            "Missing input validation",
            "security",
            &["request", "body", "DB", "parameters"],
            2,
        );
        assert_eq!(with_api.len(), 2);
        assert_eq!(with_api[0].entry.reason, e_api.reason,
            "API discriminators must rank API-reason first; got: {:?}",
            with_api[0].entry.reason);
    }

    #[test]
    fn find_similar_enriched_empty_discriminators_matches_plain() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        for t in &["SQL injection in query", "Unused import"] {
            store.record(&make_entry(t, "security", Verdict::Fp)).unwrap();
        }
        let mut index = FeedbackIndex::build_bm25(&store).unwrap();
        let plain = index.find_similar("SQL injection risk", "security", 2);
        let enriched = index.find_similar_enriched("SQL injection risk", "security", &[], 2);
        assert_eq!(plain.len(), enriched.len());
        for (p, e) in plain.iter().zip(enriched.iter()) {
            assert_eq!(p.entry.finding_title, e.entry.finding_title);
            assert!((p.similarity - e.similarity).abs() < 1e-6);
        }
    }

    #[test]
    fn build_bm25_similarity_normalized_to_unit() {
        // BM25 scores are unbounded; we normalize to [0, 1] against the top
        // hit so the existing calibrator threshold logic still applies.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        for (t, c) in &[
            ("SQL injection in auth", "security"),
            ("Unused import", "style"),
        ] {
            store.record(&make_entry(t, c, Verdict::Tp)).unwrap();
        }
        let mut index = FeedbackIndex::build_bm25(&store).unwrap();
        let similar = index.find_similar("SQL injection risk", "security", 2);
        assert!(similar[0].similarity > 0.0);
        assert!(similar[0].similarity <= 1.0, "similarity must be <= 1.0, got {}", similar[0].similarity);
    }

    #[test]
    fn build_jaccard_only_skips_embedder() {
        // --fast mode must never init fastembed: preserves ~40x RSS reduction
        // and ~100x startup-time reduction for single-shot CLI use.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        store.record(&make_entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
        let index = FeedbackIndex::build_jaccard_only(&store).unwrap();
        #[cfg(feature = "embeddings")]
        {
            assert!(index.embedder.is_none(), "fast mode must not load embedder");
            assert!(index.vectors.is_empty(), "fast mode must not compute vectors");
        }
        assert_eq!(index.entries.len(), 1);
    }

    #[test]
    fn build_jaccard_only_retrieval_works() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        store.record(&make_entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
        store.record(&make_entry("Unused import os", "style", Verdict::Fp)).unwrap();
        let mut index = FeedbackIndex::build_jaccard_only(&store).unwrap();
        let similar = index.find_similar("SQL injection in query", "security", 2);
        assert!(!similar.is_empty());
        assert!(similar[0].entry.finding_title.contains("SQL"));
    }

    #[test]
    fn jaccard_retrieval_finds_similar() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        store.record(&make_entry("SQL injection in auth", "security", Verdict::Tp)).unwrap();
        store.record(&make_entry("Unused import os", "style", Verdict::Fp)).unwrap();
        store.record(&make_entry("SQL injection via f-string", "security", Verdict::Tp)).unwrap();

        let mut index = FeedbackIndex::build(&store).unwrap();
        let similar = index.find_similar("SQL injection in query", "security", 2);
        assert!(!similar.is_empty());
        assert!(similar[0].entry.finding_title.contains("SQL"));
    }

    #[test]
    fn empty_store_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let mut index = FeedbackIndex::build(&store).unwrap();
        let similar = index.find_similar("anything", "any", 5);
        assert!(similar.is_empty());
    }

    #[cfg(feature = "embeddings")]
    #[test]
    fn rrf_preserves_distinct_duplicate_entries() {
        // Two entries with identical title/category/timestamp but different
        // verdicts (TP vs FP) must both be retrievable. The old .position()
        // lookup would misattribute both embed ranks to index 0, silently
        // dropping the FP entry's contribution.
        let dir = TempDir::new().unwrap();
        let store = FeedbackStore::new(dir.path().join("fb.jsonl"));
        let ts = Utc::now();
        // Two near-duplicates: same title/category/ts, opposite verdicts.
        let e_tp = FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "SQL injection in auth path".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "confirmed".into(),
            model: None,
            timestamp: ts,
            provenance: Provenance::Human,
        };
        let e_fp = FeedbackEntry { verdict: Verdict::Fp, reason: "false alarm".into(), ..e_tp.clone() };
        store.record(&e_tp).unwrap();
        store.record(&e_fp).unwrap();
        // Unrelated filler
        store.record(&make_entry("Unused import", "style", Verdict::Fp)).unwrap();

        let mut index = FeedbackIndex::build(&store).unwrap();
        let similar = index.find_similar("SQL injection risk", "security", 5);
        // Both near-duplicate rows should survive RRF, not collapse to one.
        let sql_count = similar.iter().filter(|s| s.entry.finding_title.contains("SQL")).count();
        assert_eq!(sql_count, 2,
            "duplicate-title entries with opposite verdicts must both be returned; got {}", sql_count);
        let has_tp = similar.iter().any(|s| s.entry.finding_title.contains("SQL") && s.entry.verdict == Verdict::Tp);
        let has_fp = similar.iter().any(|s| s.entry.finding_title.contains("SQL") && s.entry.verdict == Verdict::Fp);
        assert!(has_tp && has_fp, "both TP and FP copies must be preserved");
    }

    #[test]
    fn word_jaccard_identical_strings() {
        assert!((word_jaccard("SQL injection", "SQL injection") - 1.0).abs() < 0.001);
    }

    #[test]
    fn word_jaccard_empty_string() {
        assert_eq!(word_jaccard("", "something"), 0.0);
        assert_eq!(word_jaccard("something", ""), 0.0);
    }

    #[test]
    fn word_jaccard_partial_overlap() {
        let sim = word_jaccard("SQL injection in auth", "SQL injection via f-string");
        assert!(sim > 0.0);
        assert!(sim < 1.0);
    }

    #[test]
    fn word_jaccard_case_insensitive() {
        // Retrieval must treat title casing as equivalent; "SQL Injection" and
        // "sql injection" represent the same finding and should fully overlap.
        let sim = word_jaccard("SQL Injection in auth", "sql injection in auth");
        assert!((sim - 1.0).abs() < 1e-6, "case-insensitive overlap expected, got {}", sim);
    }
}
