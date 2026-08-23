# Context7 & Calibrator Quality Improvements

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Improve review precision by adding HA/ESPHome Context7 docs, making Context7 queries code-aware, separating wontfix from FP in the calibrator, and caching Context7 results.

**Architecture:** Four independent improvements. Task 1 is a 2-line quick win. Task 2 refactors calibrator wontfix handling. Task 3 enriches Context7 queries with import targets from hydration. Task 4 adds an LRU cache for Context7 responses.

**Tech Stack:** Rust, reqwest (existing), lru crate (new for Task 4)

---

## Task 0: Before Baseline

**Capture review output before changes for comparison.**

**Step 1: Review HA YAML files with current version**

If HA YAML files are available, run:
```bash
rtk cargo run -- review /path/to/ha/yaml --compact --no-auto-calibrate > /tmp/context7-before.txt 2>&1
```

If not, use existing test fixtures:
```bash
rtk cargo run -- review tests/fixtures/python/insecure.py --compact --no-auto-calibrate > /tmp/context7-before.txt 2>&1
rtk cargo run -- review src/pipeline.rs --compact --no-auto-calibrate >> /tmp/context7-before.txt 2>&1
```

Save for later comparison.

---

## Task 1: Add HA/ESPHome Context7 Framework Queries

**Files:**
- Modify: `src/context_enrichment.rs:18-37` (framework_queries function)

**Step 1: Write failing test**

In `src/context_enrichment.rs` test module:

```rust
#[test]
fn framework_queries_home_assistant() {
    let queries = framework_queries(&["home-assistant".into()]);
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].0, "home-assistant");
    assert!(queries[0].1.contains("automation"));
}

#[test]
fn framework_queries_esphome() {
    let queries = framework_queries(&["esphome".into()]);
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].0, "esphome");
    assert!(queries[0].1.contains("yaml"));
}
```

**Step 2: Run to verify failure**

```bash
rtk cargo test --bin quorum framework_queries_home
```

Expected: FAIL (no match arm for "home-assistant")

**Step 3: Implement**

Add to the `match` block in `framework_queries()`:

```rust
"home-assistant" => Some(("home-assistant".into(), "automations templates blueprints Jinja2 states triggers conditions actions".into())),
"esphome" => Some(("esphome".into(), "yaml components lambda sensors substitutions".into())),
```

**Step 4: Run tests**

```bash
rtk cargo test --bin quorum framework_queries
```

**Step 5: Commit**

```bash
git add src/context_enrichment.rs
git commit -m "feat(context7): add HA and ESPHome framework query mappings"
```

---

## Task 2: Separate Wontfix from FP in Calibrator

**Files:**
- Modify: `src/calibrator.rs` (both `calibrate()` and `calibrate_with_index()`)

**Problem:** Wontfix entries currently count toward `fp_weight` identically to FP. But wontfix means "real issue, accepted debt" — it should only contribute to soft suppression (INFO downgrade), not full suppression.

**Step 1: Write failing tests**

```rust
#[test]
fn wontfix_only_soft_suppresses_not_full() {
    // Wontfix alone should downgrade to INFO, not fully suppress
    let finding = FindingBuilder::new()
        .title("console.log debug artifact")
        .severity(Severity::Medium)
        .category("quality")
        .build();
    let feedback = vec![
        fb("console.log debug artifact", "quality", Verdict::Wontfix),
        fb("console.log debug artifact", "quality", Verdict::Wontfix),
        fb("console.log debug artifact", "quality", Verdict::Wontfix),
    ];
    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);

    // Should NOT be fully suppressed
    assert_eq!(result.suppressed, 0);
    assert_eq!(result.findings.len(), 1);
    // Should be soft-suppressed to INFO
    assert_eq!(result.findings[0].severity, Severity::Info);
}

#[test]
fn fp_still_fully_suppresses() {
    // Pure FP feedback should still fully suppress
    let finding = FindingBuilder::new()
        .title("console.log debug artifact")
        .severity(Severity::Medium)
        .category("quality")
        .build();
    let feedback = vec![
        fb("console.log debug artifact", "quality", Verdict::Fp),
        fb("console.log debug artifact", "quality", Verdict::Fp),
    ];
    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);

    assert_eq!(result.suppressed, 1);
    assert_eq!(result.findings.len(), 0);
}

#[test]
fn mixed_fp_wontfix_uses_fp_for_suppress_wontfix_for_soft() {
    // FP contributes to full suppression weight, wontfix to soft only
    let finding = FindingBuilder::new()
        .title("unused import")
        .severity(Severity::Medium)
        .category("quality")
        .build();
    // 1 FP (weight ~1.0) + 2 wontfix (wontfix weight only in soft bucket)
    let feedback = vec![
        fb("unused import", "quality", Verdict::Fp),
        fb("unused import", "quality", Verdict::Wontfix),
        fb("unused import", "quality", Verdict::Wontfix),
    ];
    let config = CalibratorConfig::default();
    let result = calibrate(vec![finding], &feedback, &config);

    // FP alone can't reach 1.5, so no full suppress
    // But combined soft weight (fp + wontfix) should soft suppress to INFO
    assert_eq!(result.suppressed, 0);
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, Severity::Info);
}
```

**Step 2: Run to verify failure**

```bash
rtk cargo test --bin quorum wontfix_only_soft
```

Expected: FAIL (wontfix currently counts as FP, triggers full suppress)

**Step 3: Implement**

In both `calibrate()` and `calibrate_with_index()`, split the FP weight accumulation into two buckets:

Replace:
```rust
for e in similar.iter().filter(|e| e.verdict == Verdict::Fp || e.verdict == Verdict::Wontfix) {
```

With separate loops:

```rust
// Strict FP weight (for full suppression)
let mut auto_fp_weight: f64 = 0.0;
let mut other_fp_weight: f64 = 0.0;
for e in similar.iter().filter(|e| e.verdict == Verdict::Fp) {
    if matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_)) {
        auto_fp_weight += verdict_weight(e);
    } else {
        other_fp_weight += verdict_weight(e);
    }
}
let fp_weight = auto_fp_weight.min(1.0) + other_fp_weight;

// Wontfix weight (only contributes to soft suppression)
let mut wontfix_weight: f64 = 0.0;
for e in similar.iter().filter(|e| e.verdict == Verdict::Wontfix) {
    wontfix_weight += verdict_weight(e);
}

// Combined weight for soft suppression decisions
let soft_fp_weight = fp_weight + wontfix_weight;
```

Then update the suppression logic:

```rust
// Full suppress: only strict FP weight (no wontfix)
if fp_weight >= 1.5 && fp_weight > tp_weight * 2.0 {
    finding.calibrator_action = Some(CalibratorAction::Disputed);
    suppressed += 1;
    continue;
}

// Soft suppress: FP + wontfix combined weight
if soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0 {
    finding.severity = Severity::Info;
    finding.calibrator_action = Some(CalibratorAction::Disputed);
}
```

**IMPORTANT:** Apply this change in BOTH `calibrate()` AND `calibrate_with_index()`.

For `calibrate_with_index`, the loop uses `s.entry.verdict` and `verdict_weight(&s.entry) * s.similarity as f64` — same split, just with the similarity multiplier.

**Step 4: Run tests**

```bash
rtk cargo test --bin quorum calibrat
```

Expected: all pass including existing tests (FP-only cases still fully suppress)

**Step 5: Commit**

```bash
git add src/calibrator.rs
git commit -m "fix(calibrator): separate wontfix from FP — wontfix only soft-suppresses

Wontfix means 'real issue, accepted debt' and should not trigger full
suppression. Only strict FP feedback now drives full suppression.
Wontfix contributes to soft suppression (INFO downgrade) alongside
auto-calibrate FP weight."
```

---

## Task 3: Code-Aware Context7 Queries

**Files:**
- Modify: `src/context_enrichment.rs` (add `build_code_aware_query`)
- Modify: `src/pipeline.rs` (pass import targets to query builder)

**Step 1: Write failing test**

In `src/context_enrichment.rs`:

```rust
#[test]
fn build_code_aware_query_appends_imports() {
    let base = "hooks rules component lifecycle common pitfalls";
    let imports = vec!["useEffect".to_string(), "useState".to_string(), "useCallback".to_string()];
    let query = build_code_aware_query(base, &imports);
    assert!(query.contains("hooks rules"));  // baseline preserved
    assert!(query.contains("useEffect"));
    assert!(query.contains("useState"));
}

#[test]
fn build_code_aware_query_no_imports_returns_base() {
    let base = "hooks rules component lifecycle";
    let query = build_code_aware_query(base, &[]);
    assert_eq!(query, base);
}

#[test]
fn build_code_aware_query_truncates_long_imports() {
    let base = "security validation";
    let imports: Vec<String> = (0..50).map(|i| format!("import_{}", i)).collect();
    let query = build_code_aware_query(base, &imports);
    // Should not exceed ~200 chars to keep Context7 query focused
    assert!(query.len() < 300);
    assert!(query.contains("security validation")); // baseline preserved
}
```

**Step 2: Run to verify failure**

```bash
rtk cargo test --bin quorum build_code_aware
```

**Step 3: Implement**

In `src/context_enrichment.rs`:

```rust
/// Build a code-aware Context7 query by appending relevant import targets to the baseline query.
/// Preserves the baseline to avoid context starvation, appends up to 10 import keywords.
pub fn build_code_aware_query(base_query: &str, import_targets: &[String]) -> String {
    if import_targets.is_empty() {
        return base_query.to_string();
    }
    // Extract short names from import paths (e.g., "os.path.join" -> "join")
    let keywords: Vec<&str> = import_targets.iter()
        .filter_map(|imp| imp.split(&['.', '/', ':'][..]).last())
        .filter(|s| s.len() > 2) // skip very short names
        .take(10)
        .collect();
    if keywords.is_empty() {
        return base_query.to_string();
    }
    format!("{} {}", base_query, keywords.join(" "))
}
```

Update `fetch_framework_docs` signature to accept optional imports:

```rust
pub fn fetch_framework_docs(
    frameworks: &[String],
    fetcher: &dyn ContextFetcher,
    import_targets: &[String],
) -> Vec<ContextDoc> {
    let queries = framework_queries(frameworks);
    let mut docs = Vec::new();
    for (lib_name, base_query) in queries {
        let query = build_code_aware_query(&base_query, import_targets);
        if let Some(library_id) = fetcher.resolve_library(&lib_name) {
            if let Some(content) = fetcher.query_docs(&library_id, &query, 5000) {
                docs.push(ContextDoc { library: lib_name, content });
            }
        }
    }
    docs
}
```

**Step 4: Update call sites in pipeline.rs**

In `review_file()`, the call to `fetch_framework_docs` needs the import targets. They're available from the hydration context (`ctx.import_targets`):

```rust
let docs = crate::context_enrichment::fetch_framework_docs(
    &domain.frameworks, &fetcher, &ctx.import_targets,
);
```

In `review_file_llm_only()`, there's no hydration context, so pass empty:

```rust
let docs = crate::context_enrichment::fetch_framework_docs(
    &domain.frameworks, &fetcher, &[],
);
```

**Step 5: Update existing tests that call fetch_framework_docs**

Search for all calls and add the `&[]` parameter for imports.

**Step 6: Run tests**

```bash
rtk cargo test --bin quorum
```

**Step 7: Commit**

```bash
git add src/context_enrichment.rs src/pipeline.rs
git commit -m "feat(context7): code-aware queries using import targets from hydration

Appends up to 10 import keywords to the baseline framework query.
Keeps baseline to avoid context starvation. Extracts short names
from import paths (e.g., os.path.join -> join)."
```

---

## Task 4: Context7 Response Caching

**Files:**
- Modify: `src/context_enrichment.rs` (add cache to Context7HttpFetcher)

**Step 1: Write failing test**

```rust
#[test]
fn cached_fetcher_returns_same_result() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingFetcher {
        calls: AtomicUsize,
    }
    impl ContextFetcher for CountingFetcher {
        fn resolve_library(&self, name: &str) -> Option<String> {
            Some(format!("/lib/{}", name))
        }
        fn query_docs(&self, library_id: &str, _query: &str, _max_tokens: usize) -> Option<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(format!("docs for {}", library_id))
        }
    }

    let inner = CountingFetcher { calls: AtomicUsize::new(0) };
    let cached = CachedContextFetcher::new(&inner, 16);

    // First call hits inner
    let r1 = cached.query_docs("/lib/react", "hooks", 5000);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

    // Second call with same args should be cached
    let r2 = cached.query_docs("/lib/react", "hooks", 5000);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1); // no additional call
    assert_eq!(r1, r2);

    // Different query hits inner again
    let _r3 = cached.query_docs("/lib/react", "different query", 5000);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}
```

**Step 2: Implement**

```rust
use std::collections::HashMap;
use std::sync::Mutex;

/// Caching wrapper around a ContextFetcher. Caches by (library_id, query) key.
pub struct CachedContextFetcher<'a> {
    inner: &'a dyn ContextFetcher,
    cache: Mutex<HashMap<(String, String), Option<String>>>,
    max_entries: usize,
}

impl<'a> CachedContextFetcher<'a> {
    pub fn new(inner: &'a dyn ContextFetcher, max_entries: usize) -> Self {
        Self {
            inner,
            cache: Mutex::new(HashMap::new()),
            max_entries,
        }
    }
}

impl<'a> ContextFetcher for CachedContextFetcher<'a> {
    fn resolve_library(&self, name: &str) -> Option<String> {
        // Library resolution is cheap, don't cache
        self.inner.resolve_library(name)
    }

    fn query_docs(&self, library_id: &str, query: &str, max_tokens: usize) -> Option<String> {
        let key = (library_id.to_string(), query.to_string());

        // Check cache
        if let Ok(cache) = self.cache.lock() {
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }

        // Cache miss — fetch
        let result = self.inner.query_docs(library_id, query, max_tokens);

        // Store in cache (evict oldest if full — simple clear strategy)
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() >= self.max_entries {
                cache.clear();
            }
            cache.insert(key, result.clone());
        }

        result
    }
}
```

**Step 3: Wire into pipeline.rs**

In both `review_file()` and `review_file_llm_only()`, wrap the fetcher:

```rust
let fetcher = crate::context_enrichment::Context7HttpFetcher::new();
let cached_fetcher = crate::context_enrichment::CachedContextFetcher::new(&fetcher, 32);
// Use cached_fetcher instead of fetcher
```

Actually, the cache should live across files in the review loop, not per-file. Move the fetcher creation to `run_review()` in main.rs and pass it into the pipeline functions. This requires adding a `fetcher` parameter to `PipelineConfig` or passing it separately.

Simplest approach: create the `CachedContextFetcher` in `run_review()` and pass it via `PipelineConfig`:

Add to PipelineConfig:
```rust
pub context_fetcher: Option<Box<dyn crate::context_enrichment::ContextFetcher>>,
```

This is a larger refactor. **Alternative simpler approach:** use a thread-local or lazy_static cache inside `Context7HttpFetcher` itself:

```rust
// In Context7HttpFetcher, add a cache field:
pub struct Context7HttpFetcher {
    http: reqwest::Client,
    api_key: Option<String>,
    runtime: tokio::runtime::Runtime,
    cache: Mutex<HashMap<(String, String), Option<String>>>,
}
```

Then check cache in `query_docs` before making HTTP call.

**Step 4: Run tests**

```bash
rtk cargo test --bin quorum
```

**Step 5: Commit**

```bash
git add src/context_enrichment.rs src/pipeline.rs
git commit -m "perf(context7): cache query results by (library_id, query) key

Avoids redundant HTTP calls when reviewing multiple files in the
same project with the same framework. Cache lives in Context7HttpFetcher
and persists across files in a single review run."
```

---

## Task 5: After Comparison

**Capture review output after all changes and compare with baseline.**

**Step 1: Run same reviews as Task 0**

```bash
rtk cargo run -- review tests/fixtures/python/insecure.py --compact --no-auto-calibrate > /tmp/context7-after.txt 2>&1
rtk cargo run -- review src/pipeline.rs --compact --no-auto-calibrate >> /tmp/context7-after.txt 2>&1
```

**Step 2: Compare**

```bash
diff /tmp/context7-before.txt /tmp/context7-after.txt
```

Look for:
- Fewer FP findings on HA/ESPHome files (Context7 docs helping)
- Wontfix-heavy patterns now INFO instead of suppressed
- Code-aware queries producing more relevant Context7 doc sections

**Step 3: Run stats**

```bash
rtk cargo run -- stats
```

Compare precision trend with pre-change baseline.
