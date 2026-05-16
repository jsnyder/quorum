//! A/B comparison: retrieval results with and without structural fingerprints.
//!
//! Run with: cargo test --bin quorum structural_ab -- --nocapture

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use tempfile::tempdir;

use crate::context::extract::dispatch::compute_source_fingerprints;
use crate::context::extract::fingerprint::FINGERPRINT_DIMS;
use crate::context::index::builder::IndexBuilder;
use crate::context::index::traits::{FixedClock, HashEmbedder};
use crate::context::retrieve::retriever::{RetrievalQuery, Retriever, ScoredChunk};
use crate::context::store::ChunkStore;
use crate::context::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

fn now_ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-04-20T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn mk_chunk(id: &str, source: &str, content: &str, qname: Option<&str>, lang: &str) -> Chunk {
    Chunk {
        id: id.to_string(),
        source: source.to_string(),
        kind: ChunkKind::Symbol,
        subtype: None,
        qualified_name: qname.map(str::to_string),
        signature: None,
        content: content.to_string(),
        metadata: ChunkMeta {
            source_path: format!("src/{id}.rs"),
            line_range: LineRange::new(1, 50).unwrap(),
            commit_sha: "deadbeef".to_string(),
            indexed_at: now_ts(),
            source_version: None,
            language: Some(lang.to_string()),
            is_exported: true,
            neighboring_symbols: Vec::new(),
        },
        provenance: Provenance::new("test", 0.9, "file://test").unwrap(),
    }
}

/// Build an index, insert chunks, and attach structural fingerprints.
fn setup_index(
    dir: &Path,
    chunks: &[Chunk],
    fingerprints: &HashMap<String, [f32; FINGERPRINT_DIMS]>,
) -> Connection {
    let db = dir.join("index.db");
    let clock = FixedClock::epoch();
    let emb = HashEmbedder::new(384);

    {
        let mut builder = IndexBuilder::new(&db, &clock, &emb).unwrap();
        let mut by_source: std::collections::BTreeMap<String, Vec<Chunk>> =
            std::collections::BTreeMap::new();
        for c in chunks {
            by_source
                .entry(c.source.clone())
                .or_default()
                .push(c.clone());
        }
        for (source, src_chunks) in &by_source {
            let jsonl = dir.join(format!("{source}.jsonl"));
            let mut store = ChunkStore::new(&jsonl);
            for c in src_chunks {
                store.append(c).unwrap();
            }
            builder.rebuild_from_jsonl(source, &jsonl).unwrap();
        }
        if !fingerprints.is_empty() {
            builder
                .insert_structural_fingerprints_batch(fingerprints)
                .unwrap();
        }
    }
    crate::context::index::builder::ensure_vec_loaded();
    Connection::open(&db).unwrap()
}

fn print_results(label: &str, hits: &[ScoredChunk]) {
    println!("\n--- {label} ---");
    println!(
        "{:<5} {:<30} {:<8} {:<8} {:<8} {:<8} {:<10}",
        "Rank", "Chunk ID", "Score", "BM25", "Vec", "Struct", "Legs"
    );
    println!("{}", "-".repeat(82));
    for (i, hit) in hits.iter().enumerate() {
        let legs: Vec<&str> = hit
            .source_legs
            .iter()
            .map(|l| match l {
                crate::context::retrieve::retriever::RetrievalLeg::Bm25 => "BM25",
                crate::context::retrieve::retriever::RetrievalLeg::Vector => "Vec",
                crate::context::retrieve::retriever::RetrievalLeg::Structural => "Struct",
            })
            .collect();
        println!(
            "{:<5} {:<30} {:<8.4} {:<8.4} {:<8.4} {:<8.4} {}",
            i + 1,
            &hit.chunk.id,
            hit.score,
            hit.components.bm25_norm,
            hit.components.vec_norm,
            hit.components.struct_sim,
            legs.join("+"),
        );
    }
}

/// Scenario: we're reviewing a Rust file that contains a function with error
/// handling, branching, and result-returning patterns. The index has several
/// chunks — some structurally similar (same control flow shape) but with
/// different text, and some textually similar but structurally different.
///
/// Without fingerprints, the retriever relies only on BM25 + vector similarity.
/// With fingerprints, structurally similar chunks get a boost even when their
/// text doesn't match well.
#[test]
fn ab_structural_fingerprint_ranking_impact() {
    let dir = tempdir().unwrap();

    // --- Indexed chunks: functions with varying structure and text ---

    // Chunk A: error-handling function with branches, similar STRUCTURE to query
    let chunk_a = mk_chunk(
        "validate_config",
        "repo-alpha",
        r#"pub fn validate_config(config: &Config) -> Result<ValidatedConfig, ConfigError> {
    if config.name.is_empty() {
        return Err(ConfigError::MissingField("name"));
    }
    if config.port == 0 {
        return Err(ConfigError::InvalidPort);
    }
    let resolved = resolve_paths(&config.paths)?;
    let validated = check_constraints(resolved, &config.rules)?;
    Ok(ValidatedConfig { inner: validated, source: config.clone() })
}"#,
        Some("crate::config::validate_config"),
        "rust",
    );

    // Chunk B: similar TEXT (mentions "config" and "validate") but FLAT structure
    let chunk_b = mk_chunk(
        "config_defaults",
        "repo-alpha",
        r#"pub fn config_defaults() -> Config {
    Config {
        name: "default".to_string(),
        port: 8080,
        paths: vec![],
        rules: Rules::default(),
    }
}"#,
        Some("crate::config::config_defaults"),
        "rust",
    );

    // Chunk C: different text domain (database) but SIMILAR structure to query
    // (branches, early returns, Result, error propagation)
    let chunk_c = mk_chunk(
        "validate_connection",
        "repo-beta",
        r#"pub fn validate_connection(pool: &Pool) -> Result<DbConn, DbError> {
    if pool.is_closed() {
        return Err(DbError::PoolClosed);
    }
    if pool.active_count() >= pool.max_size() {
        return Err(DbError::PoolExhausted);
    }
    let conn = pool.get_timeout(Duration::from_secs(5))?;
    let verified = conn.ping()?;
    Ok(DbConn { inner: verified, pool: pool.clone() })
}"#,
        Some("crate::db::validate_connection"),
        "rust",
    );

    // Chunk D: completely different (async, loops, no error handling)
    let chunk_d = mk_chunk(
        "poll_metrics",
        "repo-alpha",
        r#"pub async fn poll_metrics(endpoint: &str) -> Vec<Metric> {
    let mut results = Vec::new();
    for attempt in 0..3 {
        let resp = reqwest::get(endpoint).await.unwrap();
        let batch: Vec<Metric> = resp.json().await.unwrap();
        results.extend(batch);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    results
}"#,
        Some("crate::metrics::poll_metrics"),
        "rust",
    );

    // Chunk E: text overlap with "validate" but it's a simple predicate
    let chunk_e = mk_chunk(
        "is_valid_email",
        "repo-beta",
        r#"pub fn is_valid_email(s: &str) -> bool {
    s.contains('@') && s.contains('.') && s.len() > 5
}"#,
        Some("crate::util::is_valid_email"),
        "rust",
    );

    let chunks = vec![
        chunk_a.clone(),
        chunk_b.clone(),
        chunk_c.clone(),
        chunk_d.clone(),
        chunk_e.clone(),
    ];

    // --- Compute structural fingerprints for each chunk ---
    let mut fingerprints: HashMap<String, [f32; FINGERPRINT_DIMS]> = HashMap::new();
    for chunk in &chunks {
        let fps = compute_source_fingerprints(&chunk.content, "rust");
        for (_name, vec) in fps {
            fingerprints.insert(chunk.id.clone(), vec);
        }
    }

    println!(
        "\n=== Indexed {}/{} chunks with structural fingerprints ===",
        fingerprints.len(),
        chunks.len()
    );
    for (id, _) in &fingerprints {
        println!("  [fp] {id}");
    }

    let conn = setup_index(dir.path(), &chunks, &fingerprints);
    let emb = HashEmbedder::new(384);
    let clock = FixedClock(now_ts());

    // --- The "reviewed file": a validation function with similar structure to A and C ---
    let reviewed_code = r#"pub fn validate_input(input: &UserInput) -> Result<CleanInput, ValidationError> {
    if input.username.is_empty() {
        return Err(ValidationError::EmptyField("username"));
    }
    if input.age > 150 {
        return Err(ValidationError::OutOfRange("age"));
    }
    let sanitized = sanitize_html(&input.bio)?;
    let normalized = normalize_whitespace(sanitized)?;
    Ok(CleanInput { username: input.username.clone(), bio: normalized })
}"#;

    // Compute query-side fingerprints from the reviewed code.
    let query_fps = compute_source_fingerprints(reviewed_code, "rust");
    println!("\nQuery-side fingerprints computed: {}", query_fps.len());
    for (name, _) in &query_fps {
        println!("  [query fp] {name}");
    }

    // --- Query A: WITHOUT structural fingerprints (baseline) ---
    let r = Retriever::new(&conn, &emb, &clock);
    let q_baseline = RetrievalQuery {
        text: "validate input error handling".to_string(),
        identifiers: vec!["validate".to_string()],
        structural_fingerprints: vec![], // <-- no fingerprints
        k: 5,
        min_score: 0.0,
        reviewed_file_language: Some("rust".to_string()),
        ..RetrievalQuery::default()
    };
    let baseline_hits = r.query(q_baseline).unwrap();
    print_results("BASELINE (no structural fingerprints)", &baseline_hits);

    // --- Query B: WITH structural fingerprints ---
    let r2 = Retriever::new(&conn, &emb, &clock);
    let q_structural = RetrievalQuery {
        text: "validate input error handling".to_string(),
        identifiers: vec!["validate".to_string()],
        structural_fingerprints: query_fps.clone(), // <-- with fingerprints
        k: 5,
        min_score: 0.0,
        reviewed_file_language: Some("rust".to_string()),
        ..RetrievalQuery::default()
    };
    let structural_hits = r2.query(q_structural).unwrap();
    print_results("WITH STRUCTURAL FINGERPRINTS", &structural_hits);

    // --- Diff analysis ---
    println!("\n=== RANKING DELTA ===");
    let baseline_order: Vec<&str> = baseline_hits.iter().map(|h| h.chunk.id.as_str()).collect();
    let structural_order: Vec<&str> = structural_hits
        .iter()
        .map(|h| h.chunk.id.as_str())
        .collect();

    for (i, id) in structural_order.iter().enumerate() {
        let old_pos = baseline_order.iter().position(|b| b == id);
        let delta = match old_pos {
            Some(old) if old > i => format!("+{} (promoted)", old - i),
            Some(old) if old < i => format!("-{} (demoted)", i - old),
            Some(_) => "(unchanged)".to_string(),
            None => "(NEW — surfaced by fingerprint KNN)".to_string(),
        };
        let struct_sim = structural_hits[i].components.struct_sim;
        println!(
            "  #{}: {:<30} struct_sim={:.4}  {}",
            i + 1,
            id,
            struct_sim,
            delta
        );
    }

    // Verify the structural leg actually contributed something.
    let any_struct_boost = structural_hits
        .iter()
        .any(|h| h.components.struct_sim > 0.0);
    assert!(
        any_struct_boost,
        "expected at least one chunk with nonzero struct_sim"
    );

    // Verify baseline has no structural boost (ablation).
    let all_zero_baseline = baseline_hits.iter().all(|h| h.components.struct_sim == 0.0);
    assert!(
        all_zero_baseline,
        "baseline should have zero struct_sim for all chunks"
    );
}
