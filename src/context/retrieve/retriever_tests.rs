use std::path::Path;

use chrono::{DateTime, Duration, Utc};
use rusqlite::Connection;
use tempfile::tempdir;

use super::{Filters, RetrievalQuery, Retriever};
use crate::context::index::builder::IndexBuilder;
use crate::context::index::traits::{FixedClock, HashEmbedder};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn mk_chunk(
    id: &str,
    source: &str,
    content: &str,
    qname: Option<&str>,
    kind: ChunkKind,
    language: &str,
    indexed_at: DateTime<Utc>,
) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind,
        subtype: None,
        qualified_name: qname.map(str::to_string),
        signature: None,
        content: content.to_string(),
        metadata: ChunkMeta {
            source_path: format!("src/{id}.rs"),
            line_range: LineRange::new(1, 1).unwrap(),
            commit_sha: "deadbeef".to_string(),
            indexed_at,
            source_version: None,
            language: Some(language.to_string()),
            is_exported: true,
            neighboring_symbols: Vec::new(),
        },
        provenance: Provenance::new("test", 0.9, "file://test").unwrap(),
    }
}

/// Now timestamp used for test chunks; also the `FixedClock` time for query.
fn now_ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-04-20T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn build_retriever_fixture(dir: &Path, chunks: Vec<Chunk>) -> Connection {
    let db = dir.join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    {
        let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
        let mut by_source: std::collections::BTreeMap<String, Vec<Chunk>> =
            std::collections::BTreeMap::new();
        for c in chunks {
            by_source.entry(c.source.clone()).or_default().push(c);
        }
        for (source, src_chunks) in &by_source {
            let jsonl = dir.join(format!("{source}.jsonl"));
            let mut store = ChunkStore::new(&jsonl);
            for c in src_chunks {
                store.append(c).unwrap();
            }
            builder.rebuild_from_jsonl(source, &jsonl).unwrap();
        }
    }
    Connection::open(&db).unwrap()
}

/// Build a `Retriever` whose clock is anchored at `now_ts()`. Returns the
/// connection + embedder + clock so the caller can hold them for borrowing.
fn mk_retriever_ctx(dir: &Path, chunks: Vec<Chunk>) -> (Connection, HashEmbedder, FixedClock) {
    let conn = build_retriever_fixture(dir, chunks);
    (conn, HashEmbedder::new(384), FixedClock(now_ts()))
}

#[test]
fn query_returns_empty_when_no_matches() {
    // Empty DB: neither leg can produce candidates.
    let dir = tempdir().unwrap();
    let (conn, emb, clock) = mk_retriever_ctx(dir.path(), vec![]);
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        identifiers: vec!["zzznomatchxyz".into()],
        text: "nothing matches this".into(),
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn query_returns_empty_when_below_threshold() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![mk_chunk(
            "a",
            "s",
            "verify_token jwt signing",
            Some("verify_token"),
            ChunkKind::Symbol,
            "rust",
            n,
        )],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        identifiers: vec!["verify_token".into()],
        text: "jwt".into(),
        k: 10,
        min_score: 10.0,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn exact_identifier_match_outranks_semantic_only_match() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "a",
                "s",
                "unrelated prose about the weather",
                Some("verify_token"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "b",
                "s",
                "jwt validation and signing implementation",
                Some("other_fn"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        identifiers: vec!["verify_token".into()],
        text: "jwt validation".into(),
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    let pos_a = ids.iter().position(|&i| i == "a").expect("a should appear");
    let pos_b = ids.iter().position(|&i| i == "b");
    if let Some(pb) = pos_b {
        assert!(pos_a < pb, "a should rank before b: {:?}", ids);
    }
}

#[test]
fn respects_source_filter() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "a1",
                "A",
                "token flow",
                Some("a1"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "b1",
                "B",
                "token flow",
                Some("b1"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        structural_names: vec![],
        text: "token".into(),
        filters: Filters {
            sources: vec!["A".into()],
            kinds: vec![],
            exclude_source_paths: vec![],
        },
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(!hits.is_empty());
    for h in &hits {
        assert_eq!(h.chunk.source, "A");
    }
}

#[test]
fn respects_kind_filter() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "sym",
                "s",
                "token rotation",
                Some("sym"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "doc",
                "s",
                "token rotation",
                Some("doc"),
                ChunkKind::Doc,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        structural_names: vec![],
        text: "token".into(),
        filters: Filters {
            sources: vec![],
            kinds: vec![ChunkKind::Doc],
            exclude_source_paths: vec![],
        },
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(!hits.is_empty());
    for h in &hits {
        assert_eq!(h.chunk.kind, ChunkKind::Doc);
    }
}

#[test]
fn language_match_boost_applies() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "rs",
                "s",
                "alpha content token",
                Some("rs"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "py",
                "s",
                "alpha content token",
                Some("py"),
                ChunkKind::Symbol,
                "python",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "alpha content token".into(),
        reviewed_file_language: Some("rust".into()),
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].chunk.id, "rs");
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn recency_decay_applies() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let old = n - Duration::days(3650); // ~10 years old
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "new",
                "s",
                "alpha beta gamma",
                Some("new"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "old",
                "s",
                "alpha beta gamma",
                Some("old"),
                ChunkKind::Symbol,
                "rust",
                old,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "alpha beta gamma".into(),
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].chunk.id, "new");
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn k_caps_returned_hits() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let chunks: Vec<Chunk> = (0..10)
        .map(|i| {
            mk_chunk(
                &format!("c{i}"),
                "s",
                "lookup lookup lookup alpha",
                Some(&format!("c{i}")),
                ChunkKind::Symbol,
                "rust",
                n,
            )
        })
        .collect();
    let (conn, emb, clock) = mk_retriever_ctx(dir.path(), chunks);
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "lookup alpha".into(),
        k: 3,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert_eq!(hits.len(), 3);
}

#[test]
fn score_components_exposed_in_breakdown() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![mk_chunk(
            "a",
            "s",
            "jwt token validation logic",
            Some("verify_token"),
            ChunkKind::Symbol,
            "rust",
            n,
        )],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        identifiers: vec!["verify_token".into()],
        text: "jwt token".into(),
        k: 5,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(!hits.is_empty());
    let br = &hits[0].components;
    assert!((0.0..=1.0).contains(&br.bm25_norm));
    assert!((0.0..=1.0).contains(&br.vec_norm));
    assert!(
        br.recency_mul >= 0.25,
        "recency below floor: {}",
        br.recency_mul
    );
    assert!(br.score.is_finite());
    assert_eq!(br.score, hits[0].score);
}

#[test]
fn bm25_and_vec_only_candidates_both_appear() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    // A: id match only (content unrelated to text)
    // B: semantic match only (no id match, content aligned with text)
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "a",
                "s",
                "pomegranate pomegranate pomegranate",
                Some("verify_token"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "b",
                "s",
                "jwt authentication signing implementation details",
                Some("other_fn"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        identifiers: vec!["verify_token".into()],
        text: "jwt authentication".into(),
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    assert!(ids.contains(&"a"), "a missing from hits: {:?}", ids);
    // a gets +1.0 id_boost, should rank first
    assert_eq!(hits[0].chunk.id, "a");
}

#[test]
fn empty_query_returns_empty() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![mk_chunk(
            "a",
            "s",
            "alpha",
            Some("a"),
            ChunkKind::Symbol,
            "rust",
            n,
        )],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery::default();
    let hits = r.query(q).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn large_candidate_set_hydrates_via_batching() {
    // Build an index with >600 chunks so that the BM25+vector union of
    // candidate ids comfortably exceeds 500 — the IN-clause batch size.
    // k=200 is chosen so vec0's k=800 overfetch stays under its 4096 cap.
    // Success here proves the batched `IN` hydration works; a single-batch
    // implementation would also pass, but a bound-limit regression (>999)
    // would fail with "too many SQL variables".
    let dir = tempdir().unwrap();
    let n = now_ts();
    let chunks: Vec<Chunk> = (0..700)
        .map(|i| {
            mk_chunk(
                &format!("id{i:04}"),
                "s",
                &format!("batch content chunk {i}"),
                Some(&format!("id{i:04}")),
                ChunkKind::Symbol,
                "rust",
                n,
            )
        })
        .collect();
    let (conn, emb, clock) = mk_retriever_ctx(dir.path(), chunks);
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "batch content chunk".into(),
        k: 200,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert!(!hits.is_empty(), "expected non-empty hits");
    assert!(hits.len() <= 200);
}

#[test]
fn duplicate_chunk_ids_dont_inflate_results() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    // 3 chunks that will match BOTH legs (BM25 from text, vec from embedding
    // of the same text). k=3 should still return exactly 3.
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "a",
                "s",
                "unique-token-alpha content",
                Some("a"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "b",
                "s",
                "unique-token-alpha content",
                Some("b"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "c",
                "s",
                "unique-token-alpha content",
                Some("c"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "unique-token-alpha content".into(),
        k: 3,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    assert_eq!(hits.len(), 3);
    let mut ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec!["a", "b", "c"]);
}

#[test]
fn respects_exclude_source_paths_filter() {
    // Regression guard for #42 phase 1: retrieval must not return chunks
    // whose `source_path` appears in `filters.exclude_source_paths`. This
    // is how the pipeline keeps the file-under-review out of its own
    // context block — otherwise retrieval collapses the review target
    // and the reference material into one blurred prompt.
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            // Both chunks match "token flow" by BM25 + vector; only the
            // exclude filter should break the tie.
            mk_chunk(
                "under_review",
                "S",
                "token flow",
                Some("under_review"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "peer",
                "S",
                "token flow",
                Some("peer"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        structural_names: vec![],
        text: "token flow".into(),
        filters: Filters {
            sources: vec![],
            kinds: vec![],
            exclude_source_paths: vec!["src/under_review.rs".into()],
        },
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    assert!(
        !ids.contains(&"under_review"),
        "retrieval returned the excluded file: {:?}",
        ids
    );
    assert!(
        ids.contains(&"peer"),
        "retrieval should still surface sibling chunks: {:?}",
        ids
    );
}

#[test]
fn structural_names_surface_callee_definitions_even_without_similarity_match() {
    // The reviewer calls `validate`. Today's BM25 + vector might surface
    // lexically/semantically similar code but miss the actual callee.
    // Structural retrieval must surface the `validate` definition.
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk(
                "validate_def",
                "S",
                "fn validate(x: &str) -> bool { !x.is_empty() }",
                Some("validate"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            mk_chunk(
                "distractor",
                "S",
                "orchestrate the pipeline and flow control",
                Some("orchestrate"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "orchestrate pipeline flow".into(),
        structural_names: vec!["validate".into()],
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    assert!(
        ids.contains(&"validate_def"),
        "structural leg should surface the callee definition: {:?}",
        ids
    );
}

#[test]
fn structural_names_respect_exclude_source_paths() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![mk_chunk(
            "under_review",
            "S",
            "fn validate() {}",
            Some("validate"),
            ChunkKind::Symbol,
            "rust",
            n,
        )],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "irrelevant".into(),
        structural_names: vec!["validate".into()],
        filters: Filters {
            sources: vec![],
            kinds: vec![],
            exclude_source_paths: vec!["src/under_review.rs".into()],
        },
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    assert!(
        !ids.contains(&"under_review"),
        "structural leg must also respect exclude_source_paths: {:?}",
        ids
    );
}

#[test]
fn structural_query_is_deterministic() {
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            mk_chunk("a", "S", "aa", Some("a"), ChunkKind::Symbol, "rust", n),
            mk_chunk("b", "S", "bb", Some("b"), ChunkKind::Symbol, "rust", n),
            mk_chunk("c", "S", "cc", Some("c"), ChunkKind::Symbol, "rust", n),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let run_once = || -> Vec<String> {
        let q = RetrievalQuery {
            text: "aa bb cc".into(),
            structural_names: vec!["a".into(), "b".into(), "c".into()],
            k: 10,
            ..RetrievalQuery::default()
        };
        r.query(q)
            .unwrap()
            .into_iter()
            .map(|h| h.chunk.id)
            .collect()
    };
    let reference = run_once();
    for _ in 0..5 {
        assert_eq!(run_once(), reference, "output order must be stable");
    }
}

#[test]
fn structural_only_hit_survives_topk_against_strong_bm25_competitors() {
    // The decisive calibration test. Structural retrieval is
    // valuable only if its hits can survive top-K when BM25 is
    // simultaneously scoring real competitors highly.
    let dir = tempdir().unwrap();
    let n = now_ts();
    // Bulk up BM25 competitors to 6, then request top-2 so not
    // everything trivially fits. Structural_target has to EARN
    // its slot against the highest-scoring BM25 hits.
    let mut extra = Vec::new();
    for i in 0..6 {
        extra.push(mk_chunk(
            &format!("bm25_bulk_{i}"),
            "S",
            // Each repeats every query term multiple times so BM25
            // scores them very high.
            "orchestrate orchestrate pipeline pipeline flow flow tokens tokens bytes bytes flags flags",
            Some(&format!("extra_{i}")),
            ChunkKind::Symbol,
            "rust",
            n,
        ));
    }
    let (conn, emb, clock) = mk_retriever_ctx(dir.path(), {
        let mut v = vec![mk_chunk(
            "structural_target",
            "S",
            "fn validate(x: &str) -> bool { !x.is_empty() }",
            Some("validate"),
            ChunkKind::Symbol,
            "rust",
            n,
        )];
        v.extend(extra);
        v
    });
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "orchestrate pipeline flow tokens bytes flags".into(),
        structural_names: vec!["validate".into()],
        k: 2,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let ids: Vec<&str> = hits.iter().map(|h| h.chunk.id.as_str()).collect();
    assert!(
        ids.contains(&"structural_target"),
        "structural-only hit must survive top-K even when BM25 has strong \
         competitors — otherwise the id_exact_match boost is too weak and \
         the retrieval leg is effectively dead code. Got ranking: {:?}",
        ids
    );
}

#[test]
fn scored_chunk_carries_source_leg_provenance() {
    // Behavior contract: every returned ScoredChunk records which
    // leg(s) surfaced it. Single-leg and multi-leg cases both need
    // to roundtrip correctly so downstream telemetry can answer
    // "did structural-only hits survive?" without guessing.
    use crate::context::retrieve::retriever::RetrievalLeg;
    let dir = tempdir().unwrap();
    let n = now_ts();
    let (conn, emb, clock) = mk_retriever_ctx(
        dir.path(),
        vec![
            // Structural only — qname matches, content doesn't.
            mk_chunk(
                "s_only",
                "S",
                "zzzunrelatedzzz",
                Some("target"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            // BM25 only — matches query text, qname doesn't.
            mk_chunk(
                "b_only",
                "S",
                "alpha beta gamma delta",
                Some("bm25_fn"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
            // Both — matches query text AND has the target qname.
            mk_chunk(
                "s_and_b",
                "S",
                "alpha beta gamma delta",
                Some("target"),
                ChunkKind::Symbol,
                "rust",
                n,
            ),
        ],
    );
    let r = Retriever::new(&conn, &emb, &clock);
    let q = RetrievalQuery {
        text: "alpha beta gamma delta".into(),
        structural_names: vec!["target".into()],
        k: 10,
        ..RetrievalQuery::default()
    };
    let hits = r.query(q).unwrap();
    let by_id: std::collections::HashMap<_, _> =
        hits.iter().map(|h| (h.chunk.id.as_str(), h)).collect();

    let s_only = by_id.get("s_only").expect("s_only should appear");
    assert!(
        s_only.source_legs.contains(&RetrievalLeg::Structural),
        "s_only must carry Structural tag; got {:?}",
        s_only.source_legs
    );
    assert!(
        !s_only.source_legs.contains(&RetrievalLeg::Bm25),
        "s_only must NOT carry Bm25 tag (content doesn't match query); got {:?}",
        s_only.source_legs
    );

    let b_only = by_id.get("b_only").expect("b_only should appear");
    assert!(
        b_only.source_legs.contains(&RetrievalLeg::Bm25),
        "b_only must carry Bm25 tag; got {:?}",
        b_only.source_legs
    );
    assert!(
        !b_only.source_legs.contains(&RetrievalLeg::Structural),
        "b_only must NOT carry Structural tag (qname doesn't match); got {:?}",
        b_only.source_legs
    );

    let both = by_id.get("s_and_b").expect("s_and_b should appear");
    assert!(
        both.source_legs.contains(&RetrievalLeg::Bm25)
            && both.source_legs.contains(&RetrievalLeg::Structural),
        "multi-leg hit must carry both Bm25 and Structural; got {:?}",
        both.source_legs
    );
}
