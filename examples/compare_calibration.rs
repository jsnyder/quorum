//! A/B: hold findings constant, run the calibrator with RRF on vs off.
//! Measures how often each mode suppresses, boosts, or diverges in action.
use bm25::{Language, SearchEngineBuilder};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::PathBuf;

#[derive(serde::Deserialize, Debug, Clone)]
struct Entry {
    finding_title: String,
    #[serde(default)]
    #[allow(dead_code)]
    finding_category: String,
    #[serde(default)]
    verdict: Option<String>,
}

fn load_entries(path: &std::path::Path) -> anyhow::Result<Vec<Entry>> {
    let txt = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in txt.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(e) = serde_json::from_str::<Entry>(line)
            && !e.finding_title.is_empty() {
                out.push(e);
            }
    }
    Ok(out)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let mut d = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        d += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    d / ((na.sqrt() * nb.sqrt()).max(1e-9))
}

/// Return ranked entry indices, highest similarity first.
fn embed_rank(qv: &[f32], corpus_vecs: &[Vec<f32>], top_n: usize) -> Vec<(usize, f32)> {
    let mut scored: Vec<(usize, f32)> = corpus_vecs
        .iter()
        .enumerate()
        .map(|(i, v)| (i, cosine(qv, v)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_n);
    scored
}

/// RRF fuse two rank lists. Returns top-K with normalized similarity (top = 1.0).
fn rrf(bm25_ranked: &[usize], embed_ranked: &[usize], top_k: usize) -> Vec<(usize, f32)> {
    const K: f32 = 60.0;
    let mut scores: std::collections::HashMap<usize, f32> = std::collections::HashMap::new();
    for (r, idx) in bm25_ranked.iter().enumerate() {
        *scores.entry(*idx).or_insert(0.0) += 1.0 / (K + r as f32 + 1.0);
    }
    for (r, idx) in embed_ranked.iter().enumerate() {
        *scores.entry(*idx).or_insert(0.0) += 1.0 / (K + r as f32 + 1.0);
    }
    let mut fused: Vec<(usize, f32)> = scores.into_iter().collect();
    fused.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let max = fused.first().map(|(_, s)| *s).unwrap_or(1e-6).max(1e-6);
    fused
        .into_iter()
        .take(top_k)
        .map(|(i, s)| (i, (s / max).clamp(0.0, 1.0)))
        .collect()
}

/// Simulate the calibrator's TP/FP weight accumulation for a given set of
/// precedent indices and verdicts. Returns (tp_weight, fp_weight).
fn calibrate(indices: &[usize], entries: &[Entry], similarities: &[f32]) -> (f64, f64) {
    let mut tp = 0.0;
    let mut fp = 0.0;
    for (k, idx) in indices.iter().enumerate() {
        let sim = similarities.get(k).copied().unwrap_or(1.0) as f64;
        if sim < 0.80 {
            continue;
        }
        let e = &entries[*idx];
        match e.verdict.as_deref() {
            Some("tp") | Some("partial") => tp += 1.0 * sim,
            Some("fp") => fp += 1.0 * sim,
            _ => {}
        }
    }
    (tp, fp)
}

fn action(tp: f64, fp: f64) -> &'static str {
    // Matches the production calibrator's two-tier logic roughly:
    //  - strong FP dominates TP → disputed
    //  - strong TP dominates FP → confirmed
    //  - else none
    let soft_fp = fp;
    if (soft_fp >= 1.0 && soft_fp > tp * 2.0) || (soft_fp >= 0.5 && tp < 0.1) {
        "disputed"
    } else if tp >= 1.5 && tp > fp * 2.0 {
        "confirmed_boost"
    } else if tp > fp * 1.5 {
        "confirmed"
    } else {
        "none"
    }
}

fn main() -> anyhow::Result<()> {
    let home = std::env::var("HOME").unwrap();
    let path = PathBuf::from(home).join(".quorum/feedback.jsonl");
    let entries = load_entries(&path)?;
    println!("Loaded {} entries", entries.len());

    // Fixed query set: 40 realistic recent findings
    let queries: Vec<(&str, &str)> = vec![
        (
            "Function `check_vendor_urls` has cyclomatic complexity 11",
            "complexity",
        ),
        (
            "Function `gameLoop` has cyclomatic complexity 19",
            "complexity",
        ),
        ("Function `pipe` has cyclomatic complexity 30", "complexity"),
        (
            "Function `createFood` has cyclomatic complexity 13",
            "complexity",
        ),
        (
            "Function `checkCollisions` has cyclomatic complexity 10",
            "complexity",
        ),
        (
            "Catch-all `except: pass` silently swallows errors",
            "error-handling",
        ),
        ("console.log debug artifact left in code", "style"),
        ("SQL injection risk in query builder", "security"),
        (
            "`open()` without explicit `encoding` parameter",
            "reliability",
        ),
        ("Unused import increases noise", "style"),
        ("Image load failure causes endless polling loop", "logic"),
        (
            "Canvas lookup and 2D context used without null checks",
            "reliability",
        ),
        (
            "Missing error handling on subprocess call",
            "error-handling",
        ),
        (
            "Schema conversion assumes properties always exist",
            "robustness",
        ),
        (
            "Broad exception catch hides network failures",
            "error-handling",
        ),
        ("Hardcoded URL in configuration", "config"),
        ("State is never reset between requests", "state-management"),
        (
            "Mutable default argument retains state across calls",
            "python",
        ),
        (
            "Race condition on shared counter without lock",
            "concurrency",
        ),
        ("Unvalidated redirect destination", "security"),
        ("Bare except masks KeyboardInterrupt", "error-handling"),
        ("Deprecated use of datetime.utcnow()", "deprecation"),
        ("Input path validation accepts directories", "robustness"),
        ("Blocking input() in async code freezes event loop", "async"),
        ("Only first tool call from response is executed", "logic"),
        (
            "Restarting game creates additional concurrent loops",
            "state-management",
        ),
        ("Floor game-over unreachable due to clamping", "logic"),
        ("Exception details disclosed in API response", "security"),
        (
            "Search result titles injected into Markdown without escaping",
            "security",
        ),
        (
            "Naive/aware datetime mixing in timezone calculation",
            "correctness",
        ),
        ("YAML duplicate keys cause silent override", "correctness"),
        (
            "Docker FROM :latest causes non-reproducible builds",
            "infrastructure",
        ),
        ("Wildcard IAM permission in terraform config", "security"),
        ("Missing HEALTHCHECK in Dockerfile", "reliability"),
        ("Bash curl | bash without verification", "security"),
        (
            "Terraform security group opens port 22 to 0.0.0.0/0",
            "security",
        ),
        ("Non-threadsafe singleton pattern", "concurrency"),
        ("Mutation during iteration in Python dict", "correctness"),
        ("Jinja template list accumulation in loop", "template"),
        ("YAML boolean coercion of 'yes'/'no' strings", "correctness"),
    ];

    let titles: Vec<String> = entries.iter().map(|e| e.finding_title.clone()).collect();

    println!("Building BM25...");
    let bm25 = SearchEngineBuilder::<u32>::with_corpus(Language::English, titles.clone()).build();

    println!("Loading fastembed + corpus vectors...");
    let mut embedder = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::BGESmallENV15))?;
    let corpus_vecs = embedder.embed(titles.clone(), None)?;

    let top_k = 10;
    let pool = 30;

    let mut action_agreement = 0;
    let mut only_e: std::collections::HashMap<&'static str, usize> = Default::default();
    let mut only_r: std::collections::HashMap<&'static str, usize> = Default::default();
    let mut both_act: std::collections::HashMap<(&'static str, &'static str), usize> =
        Default::default();

    for (qt, _qc) in &queries {
        let qv = embedder
            .embed(vec![qt.to_string()], None)?
            .into_iter()
            .next()
            .unwrap();
        let e_ranked = embed_rank(&qv, &corpus_vecs, pool);
        let b_results = bm25.search(qt, pool);
        let b_ranked: Vec<usize> = b_results.iter().map(|r| r.document.id as usize).collect();
        let e_ranked_idx: Vec<usize> = e_ranked.iter().map(|(i, _)| *i).collect();

        // Pure embedding top-K with cosine sims
        let e_top: Vec<usize> = e_ranked.iter().take(top_k).map(|(i, _)| *i).collect();
        let e_sims: Vec<f32> = e_ranked.iter().take(top_k).map(|(_, s)| *s).collect();

        // RRF fused top-K with normalized similarities (matches production).
        let r_with_sims = rrf(&b_ranked, &e_ranked_idx, top_k);
        let r_top: Vec<usize> = r_with_sims.iter().map(|(i, _)| *i).collect();
        let r_sims: Vec<f32> = r_with_sims.iter().map(|(_, s)| *s).collect();

        let (tp_e, fp_e) = calibrate(&e_top, &entries, &e_sims);
        let (tp_r, fp_r) = calibrate(&r_top, &entries, &r_sims);
        let act_e = action(tp_e, fp_e);
        let act_r = action(tp_r, fp_r);

        *both_act.entry((act_e, act_r)).or_insert(0) += 1;
        if act_e == act_r {
            action_agreement += 1;
        } else {
            *only_e.entry(act_e).or_insert(0) += 1;
            *only_r.entry(act_r).or_insert(0) += 1;
            println!("\nDIVERGE: {}", qt);
            println!("  EMBED: tp={:.2} fp={:.2} → {}", tp_e, fp_e, act_e);
            println!("  RRF:   tp={:.2} fp={:.2} → {}", tp_r, fp_r, act_r);
        }
    }

    println!("\n================================================");
    println!("Query set: {}", queries.len());
    println!(
        "Action agreement between EMBED and RRF: {}/{}",
        action_agreement,
        queries.len()
    );
    println!("\nConfusion matrix (EMBED_action → RRF_action : count):");
    let mut sorted: Vec<_> = both_act.iter().collect();
    sorted.sort_by_key(|(k, _)| (k.0, k.1));
    for ((e, r), n) in sorted {
        let tag = if e == r { "==" } else { "DIFF" };
        println!("  {:<18} → {:<18}  : {:>2}  [{}]", e, r, n, tag);
    }
    Ok(())
}
