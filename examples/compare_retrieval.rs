//! Benchmark: compare top-K retrieval quality across Jaccard / BM25 / fastembed
//! on the real ~/.quorum/feedback.jsonl store. Self-contained (no quorum deps).
//!
//! Run: cargo run --release --example compare_retrieval
use bm25::{Language, SearchEngineBuilder};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

#[derive(serde::Deserialize, Debug, Clone)]
struct Entry {
    finding_title: String,
    #[serde(default)]
    finding_category: String,
}

fn load_entries(path: &std::path::Path) -> anyhow::Result<Vec<Entry>> {
    let txt = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in txt.lines() {
        if line.trim().is_empty() { continue; }
        if let Ok(e) = serde_json::from_str::<Entry>(line) {
            if !e.finding_title.is_empty() { out.push(e); }
        }
    }
    Ok(out)
}

fn word_set(s: &str) -> HashSet<String> {
    s.split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

fn jaccard(a: &str, b: &str) -> f64 {
    let sa = word_set(a);
    let sb = word_set(b);
    let i = sa.intersection(&sb).count() as f64;
    let u = sa.union(&sb).count() as f64;
    if u == 0.0 { 0.0 } else { i / u }
}

fn jaccard_top_k(query: &str, corpus: &[String], k: usize) -> Vec<String> {
    let mut scored: Vec<(f64, &String)> = corpus.iter().map(|d| (jaccard(query, d), d)).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(k).map(|(_, s)| s.clone()).collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() { dot += a[i]*b[i]; na += a[i]*a[i]; nb += b[i]*b[i]; }
    let denom = (na.sqrt() * nb.sqrt()).max(1e-9);
    dot / denom
}

fn embed_top_k(query_vec: &[f32], corpus_vecs: &[Vec<f32>], corpus_titles: &[String], k: usize) -> Vec<String> {
    let mut scored: Vec<(f32, &String)> = corpus_vecs.iter().enumerate().map(|(i, v)| (cosine(query_vec, v), &corpus_titles[i])).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(k).map(|(_, s)| s.clone()).collect()
}

/// RRF fusion of two ranked lists (k=60). Takes rank (1-indexed).
fn rrf_fuse(bm25_ranked: &[String], embed_ranked: &[String], top_k: usize) -> Vec<String> {
    const K: f32 = 60.0;
    let mut scores: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for (rank, title) in bm25_ranked.iter().enumerate() {
        *scores.entry(title.clone()).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
    }
    for (rank, title) in embed_ranked.iter().enumerate() {
        *scores.entry(title.clone()).or_insert(0.0) += 1.0 / (K + rank as f32 + 1.0);
    }
    let mut fused: Vec<(String, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    fused.into_iter().take(top_k).map(|(s, _)| s).collect()
}

fn set_overlap(a: &[String], b: &[String]) -> f64 {
    let sa: HashSet<&String> = a.iter().collect();
    let sb: HashSet<&String> = b.iter().collect();
    let i = sa.intersection(&sb).count() as f64;
    let u = sa.union(&sb).count() as f64;
    if u == 0.0 { 0.0 } else { i / u }
}

fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap();
    let path = PathBuf::from(home).join(".quorum/feedback.jsonl");
    println!("Loading {}", path.display());
    let entries = load_entries(&path)?;
    println!("Loaded {} entries", entries.len());

    let titles: Vec<String> = entries.iter().map(|e| e.finding_title.clone()).collect();

    // Queries — real recent findings
    let queries: Vec<&str> = vec![
        "Function `check_vendor_urls` has cyclomatic complexity 11",
        "Function `gameLoop` has cyclomatic complexity 19",
        "Function `pipe` has cyclomatic complexity 30",
        "Catch-all `except: pass` silently swallows errors",
        "console.log debug artifact left in code",
        "SQL injection risk in query builder",
        "`open()` without explicit `encoding` parameter",
        "Unused import increases noise",
        "Image load failure causes endless polling loop",
        "Canvas lookup and 2D context used without null checks",
        "Missing error handling on subprocess call",
        "Schema conversion assumes properties always exist",
        "Broad exception catch hides network failures",
        "Hardcoded URL in configuration",
        "State is never reset between requests",
        "Mutable default argument retains state across calls",
        "Race condition on shared counter without lock",
        "Unvalidated redirect destination",
        "Bare except masks KeyboardInterrupt",
        "Deprecated use of datetime.utcnow()",
    ];

    // --- Build indices ---
    let t0 = Instant::now();
    let bm25_engine = SearchEngineBuilder::<u32>::with_corpus(Language::English, titles.clone()).build();
    let bm25_build_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let mut embedder = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))?;
    let embedder_init_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let corpus_vecs: Vec<Vec<f32>> = embedder.embed(titles.clone(), None)?;
    let embed_build_ms = t0.elapsed().as_millis();

    let k: usize = 5;
    let n = queries.len() as f64;

    let mut tot_j = 0u128;
    let mut tot_b = 0u128;
    let mut tot_e = 0u128;
    let mut tot_r = 0u128;
    let mut jb = 0.0f64; let mut je = 0.0f64; let mut be_ = 0.0f64;
    let mut re_ = 0.0f64; let mut rb = 0.0f64;
    let mut jb_t1 = 0; let mut je_t1 = 0; let mut be_t1 = 0;
    let mut re_t1 = 0; let mut rb_t1 = 0;

    for q in &queries {
        let t0 = Instant::now();
        let j_top = jaccard_top_k(q, &titles, k);
        tot_j += t0.elapsed().as_micros();

        let t0 = Instant::now();
        let b_res = bm25_engine.search(q, k);
        tot_b += t0.elapsed().as_micros();
        let b_top: Vec<String> = b_res.iter().map(|r| r.document.contents.clone()).collect();

        let t0 = Instant::now();
        let q_vec = embedder.embed(vec![q.to_string()], None)?.into_iter().next().unwrap();
        let e_top = embed_top_k(&q_vec, &corpus_vecs, &titles, k);
        tot_e += t0.elapsed().as_micros();

        // RRF fusion of BM25 + embedding top-20 pools
        let t0 = Instant::now();
        let b_pool = bm25_engine.search(q, 20);
        let b_pool_titles: Vec<String> = b_pool.iter().map(|r| r.document.contents.clone()).collect();
        let e_pool = embed_top_k(&q_vec, &corpus_vecs, &titles, 20);
        let r_top = rrf_fuse(&b_pool_titles, &e_pool, k);
        tot_r += t0.elapsed().as_micros();

        jb += set_overlap(&j_top, &b_top);
        je += set_overlap(&j_top, &e_top);
        be_ += set_overlap(&b_top, &e_top);
        re_ += set_overlap(&r_top, &e_top);
        rb += set_overlap(&r_top, &b_top);
        if !j_top.is_empty() && !b_top.is_empty() && j_top[0] == b_top[0] { jb_t1 += 1; }
        if !j_top.is_empty() && !e_top.is_empty() && j_top[0] == e_top[0] { je_t1 += 1; }
        if !b_top.is_empty() && !e_top.is_empty() && b_top[0] == e_top[0] { be_t1 += 1; }
        if !r_top.is_empty() && !e_top.is_empty() && r_top[0] == e_top[0] { re_t1 += 1; }
        if !r_top.is_empty() && !b_top.is_empty() && r_top[0] == b_top[0] { rb_t1 += 1; }

        println!("\nQ: {}", q);
        println!("  J: {}", j_top.first().map(|s| s.as_str()).unwrap_or(""));
        println!("  B: {}", b_top.first().map(|s| s.as_str()).unwrap_or(""));
        println!("  E: {}", e_top.first().map(|s| s.as_str()).unwrap_or(""));
        println!("  R: {}", r_top.first().map(|s| s.as_str()).unwrap_or(""));
    }

    println!("\n=================================================");
    println!("Build:  BM25={}ms  embedder_init={}ms  embed_corpus={}ms", bm25_build_ms, embedder_init_ms, embed_build_ms);
    println!("\nAvg query latency ({} queries):", queries.len());
    println!("  Jaccard   : {} µs", tot_j / n as u128);
    println!("  BM25      : {} µs", tot_b / n as u128);
    println!("  Embedding : {} µs  (includes per-query encode)", tot_e / n as u128);
    println!("  RRF (B+E) : {} µs  (both retrievals + fuse)", tot_r / n as u128);

    println!("\nTop-5 set overlap (higher = more agreement):");
    println!("  Jaccard ↔ BM25      : {:.2}", jb / n);
    println!("  Jaccard ↔ Embedding : {:.2}", je / n);
    println!("  BM25    ↔ Embedding : {:.2}", be_ / n);
    println!("  RRF     ↔ Embedding : {:.2}", re_ / n);
    println!("  RRF     ↔ BM25      : {:.2}", rb / n);

    println!("\nTop-1 exact agreement (out of {}):", queries.len());
    println!("  Jaccard ↔ BM25      : {}", jb_t1);
    println!("  Jaccard ↔ Embedding : {}", je_t1);
    println!("  BM25    ↔ Embedding : {}", be_t1);
    println!("  RRF     ↔ Embedding : {}", re_t1);
    println!("  RRF     ↔ BM25      : {}", rb_t1);

    Ok(())
}
