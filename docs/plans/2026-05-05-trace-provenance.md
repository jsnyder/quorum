# Trace Provenance Metadata — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add provenance metadata (version, repo, commit SHA, dirty state, model, run_id, timestamp) to every calibrator trace entry, and add fuzzy matching ablation + join-time filtering for corpus analysis.

**Architecture:** TraceProvenance struct on CalibratorConfig (no new function parameters). Nested serde object (not flattened) for schema safety. JoinFilter for corpus slicing. Binary fuzzy ablation toggle.

**Tech Stack:** Rust, serde, chrono, existing GitOps trait

---

### Task 1: TraceProvenance struct + CalibratorTraceEntry integration

**Files:**
- Modify: `src/calibrator_trace.rs`

**Step 1: Write the failing test — TraceProvenance round-trip**

```rust
#[test]
fn trace_provenance_round_trips() {
    let prov = TraceProvenance {
        quorum_version: Some("0.19.0".into()),
        repo: Some("quorum".into()),
        commit_sha: Some("abc123".into()),
        dirty: Some(false),
        review_model: Some("gpt-5.4".into()),
        run_id: Some("01JTEST".into()),
        timestamp: Some("2026-05-05T12:00:00Z".into()),
    };
    let json = serde_json::to_string(&prov).unwrap();
    let back: TraceProvenance = serde_json::from_str(&json).unwrap();
    assert_eq!(prov, back);
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --lib trace_provenance_round_trips`
Expected: FAIL — TraceProvenance does not exist

**Step 3: Write the TraceProvenance struct**

Add to `src/calibrator_trace.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TraceProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
```

Add `provenance` field to `CalibratorTraceEntry`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub provenance: Option<TraceProvenance>,
```

Update all existing test fixtures in the file to include `provenance: None`.

**Step 4: Run test to verify it passes**

Run: `rtk cargo test --lib trace_provenance_round_trips`
Expected: PASS

**Step 5: Commit**

```bash
rtk git add src/calibrator_trace.rs
git commit -m "feat(trace): add TraceProvenance struct to CalibratorTraceEntry"
```

---

### Task 2: Backward compatibility + schema strictness tests

**Files:**
- Modify: `src/calibrator_trace.rs` (tests only)

**Step 1: Write failing tests**

```rust
#[test]
fn old_trace_without_provenance_deserializes() {
    // Pre-upgrade trace line: no provenance key at all
    let json = r#"{"finding_title":"x","finding_category":"y","tp_weight":0.0,"fp_weight":0.0,"wontfix_weight":0.0,"full_suppress_weight":0.0,"soft_fp_weight":0.0,"matched_precedents":[],"action":null,"input_severity":"low","output_severity":"low"}"#;
    let trace: CalibratorTraceEntry = serde_json::from_str(json).unwrap();
    assert!(trace.provenance.is_none());
}

#[test]
fn new_trace_has_nested_provenance_object() {
    let trace = CalibratorTraceEntry {
        finding_title: "test".into(),
        finding_category: "security".into(),
        tp_weight: 0.0, fp_weight: 0.0, wontfix_weight: 0.0,
        full_suppress_weight: 0.0, soft_fp_weight: 0.0,
        matched_precedents: vec![], action: None,
        input_severity: Severity::Low, output_severity: Severity::Low,
        severity_change_reason: None, file_path: None,
        provenance: Some(TraceProvenance {
            quorum_version: Some("0.19.0".into()),
            ..Default::default()
        }),
    };
    let json = serde_json::to_string(&trace).unwrap();
    assert!(json.contains(r#""provenance":{"quorum_version":"0.19.0"}"#));
    assert!(!json.contains(r#""repo""#), "None fields should be omitted");
}

#[test]
fn provenance_rejects_unknown_keys() {
    // Schema strictness: unknown key inside provenance should fail
    let json = r#"{"quorum_version":"0.19.0","bogus_field":"oops"}"#;
    let result: Result<TraceProvenance, _> = serde_json::from_str(json);
    // serde by default ignores unknown fields — we need #[serde(deny_unknown_fields)]
    assert!(result.is_err(), "unknown keys in provenance should be rejected");
}

#[test]
fn all_none_provenance_serializes_empty() {
    let prov = TraceProvenance::default();
    let json = serde_json::to_string(&prov).unwrap();
    assert_eq!(json, "{}");
}
```

**Step 2: Run tests to verify they fail**

Run: `rtk cargo test --lib old_trace_without_provenance_deserializes new_trace_has_nested_provenance provenance_rejects_unknown all_none_provenance`
Expected: FAIL — some tests may pass if struct already exists from Task 1, but `provenance_rejects_unknown_keys` should fail without `deny_unknown_fields`

**Step 3: Add `#[serde(deny_unknown_fields)]` to TraceProvenance**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceProvenance {
    // ... fields ...
}
```

**Step 4: Run tests to verify they pass**

Run: `rtk cargo test --lib -k provenance`
Expected: PASS

**Step 5: Commit**

```bash
rtk git add src/calibrator_trace.rs
git commit -m "test(trace): backward compat + schema strictness for TraceProvenance"
```

---

### Task 3: Carry provenance on CalibratorConfig

**Files:**
- Modify: `src/calibrator.rs`

**Step 1: Write failing test — config carries provenance to traces**

```rust
#[test]
fn calibrate_attaches_provenance_to_traces() {
    let findings = vec![Finding { /* minimal finding */ }];
    let feedback = vec![];
    let prov = crate::calibrator_trace::TraceProvenance {
        quorum_version: Some("0.19.0".into()),
        repo: Some("test-repo".into()),
        ..Default::default()
    };
    let mut config = CalibratorConfig::default();
    config.trace_provenance = Some(prov.clone());
    let result = calibrate(findings, &feedback, &config, "src/test.rs");
    assert!(!result.traces.is_empty());
    for trace in &result.traces {
        assert_eq!(trace.provenance.as_ref(), Some(&prov));
    }
}
```

**Step 2: Run test to verify it fails**

Run: `rtk cargo test --lib calibrate_attaches_provenance`
Expected: FAIL — `trace_provenance` field doesn't exist on CalibratorConfig

**Step 3: Implementation**

Add to `CalibratorConfig`:

```rust
pub trace_provenance: Option<crate::calibrator_trace::TraceProvenance>,
```

Update `Default` impl: `trace_provenance: None`.

Modify `make_no_match_trace()` and `make_trace_entry()` to accept `&CalibratorConfig` (they already receive individual config values — refactor to take the full config) and read `config.trace_provenance.clone()` into the trace's `provenance` field.

Alternatively, if the helpers already receive enough params, just add the provenance from config at the call sites in `calibrate()` and `calibrate_with_index()` after trace construction:

```rust
let mut trace = make_no_match_trace(&finding, file_path);
trace.provenance = config.trace_provenance.clone();
```

This is the minimal-diff approach — no signature changes to helpers.

**Step 4: Run test to verify it passes**

Run: `rtk cargo test --lib calibrate_attaches_provenance`
Expected: PASS

**Step 5: Commit**

```bash
rtk git add src/calibrator.rs
git commit -m "feat(calibrator): carry TraceProvenance on CalibratorConfig"
```

---

### Task 4: Provenance on calibrate_with_index too

**Files:**
- Modify: `src/calibrator.rs`

**Step 1: Write failing test**

```rust
#[test]
fn calibrate_with_index_attaches_provenance() {
    // Similar to Task 3 but exercises calibrate_with_index path
    let findings = vec![Finding { /* minimal */ }];
    let mut index = FeedbackIndex::new();
    // index is empty, so all findings get NoMatch traces
    let prov = crate::calibrator_trace::TraceProvenance {
        quorum_version: Some("0.19.0".into()),
        ..Default::default()
    };
    let mut config = CalibratorConfig::default();
    config.trace_provenance = Some(prov.clone());
    let result = calibrate_with_index(findings, &mut index, &config, "src/test.rs");
    for trace in &result.traces {
        assert_eq!(trace.provenance.as_ref(), Some(&prov));
    }
}
```

**Step 2: Run test — expect FAIL**

**Step 3: Apply same pattern as Task 3 in `calibrate_with_index()`**

**Step 4: Run test — expect PASS**

**Step 5: Commit**

```bash
rtk git add src/calibrator.rs
git commit -m "feat(calibrator): propagate provenance in calibrate_with_index"
```

---

### Task 5: Populate provenance in main.rs

**Files:**
- Modify: `src/main.rs`
- Modify: `src/pipeline.rs` (if needed to thread through PipelineConfig)

**Step 1: Write failing test — integration-level**

This is harder to unit test since main.rs orchestration involves git.
Write a focused test that the provenance is set on the CalibratorConfig
that reaches the pipeline:

```rust
// In pipeline.rs tests or main.rs tests
#[test]
fn pipeline_config_default_has_no_provenance() {
    let config = PipelineConfig::default();
    assert!(config.calibrator_config.trace_provenance.is_none());
}
```

**Step 2: Run test — may pass immediately (default is None)**

**Step 3: Implementation in main.rs**

In the `review` command handler, before creating PipelineConfig:

```rust
use crate::calibrator_trace::TraceProvenance;
use crate::context::inject::stale::{SystemGit, GitOps};

let git = SystemGit;
let repo_root = std::env::current_dir().ok();
let commit_sha = repo_root.as_deref().and_then(|r| git.head_sha(r));
let dirty = repo_root.as_deref().map(|r| git.has_local_changes(r));
let repo_name = repo_root.as_deref()
    .and_then(|p| p.file_name())
    .map(|n| n.to_string_lossy().into_owned());

let trace_provenance = Some(TraceProvenance {
    quorum_version: Some(env!("CARGO_PKG_VERSION").into()),
    repo: repo_name,
    commit_sha,
    dirty,
    review_model: models.first().cloned(),
    run_id: Some(run_id.clone()),  // run_id already generated for ReviewRecord
    timestamp: Some(chrono::Utc::now().to_rfc3339()),
});

// Set on calibrator_config before building PipelineConfig
calibrator_config.trace_provenance = trace_provenance;
```

**Step 4: Verify with a manual run**

Run: `rtk cargo build && cargo run -- review src/calibrator_trace.rs --trace 2>&1 | head -20`
Check that traces in `~/.quorum/calibrator_traces.jsonl` have `provenance` field.

**Step 5: Commit**

```bash
rtk git add src/main.rs src/pipeline.rs
git commit -m "feat(main): populate TraceProvenance from git + version + model + run_id"
```

---

### Task 6: Fuzzy matching ablation flag

**Files:**
- Modify: `src/calibrator.rs` (CalibratorConfig)
- Modify: `src/calibrate.rs` (join logic)

**Step 1: Write failing test — ablation disables fuzzy tiers**

```rust
#[test]
fn join_with_fuzzy_disabled_only_uses_raw() {
    // Set up: feedback title "SQL injection" for file "src/db.rs"
    // Trace title "sql-injection: SQL injection" for file "src/db.rs"
    // Without fuzzy: raw exact doesn't match (titles differ)
    // With fuzzy: normalized match would succeed
    let feedback = vec![/* fb entry with title "SQL injection", file "src/db.rs" */];
    let traces = vec![/* trace with title "sql-injection: SQL injection", file "src/db.rs" */];
    
    // Fuzzy enabled (default): should match via normalization
    let (samples_on, stats_on) = join_feedback_and_traces(&feedback, &traces);
    assert!(stats_on.exact_normalized > 0 || stats_on.fuzzy_same_file > 0);
    
    // Fuzzy disabled: should NOT match (raw titles differ)
    let filter = JoinFilter::default();
    let (samples_off, stats_off) = join_feedback_and_traces_filtered(
        &feedback, &traces, &filter, true /* disable_fuzzy */
    );
    assert_eq!(stats_off.exact_normalized, 0);
    assert_eq!(stats_off.fuzzy_same_file, 0);
}
```

**Step 2: Run test — FAIL (function doesn't exist yet)**

**Step 3: Implementation**

Add `disable_fuzzy_matching: Option<bool>` to `CalibratorConfig` with default `None`.

Add `fuzzy_matching_disabled()` helper mirroring `calibrator_disabled()` pattern:

```rust
fn fuzzy_matching_disabled(config: &CalibratorConfig) -> bool {
    config.disable_fuzzy_matching
        .unwrap_or_else(|| std::env::var("QUORUM_DISABLE_FUZZY_MATCHING").is_ok())
}
```

Modify `join_feedback_and_traces` to accept a `disable_fuzzy: bool` parameter
(or add a new `join_feedback_and_traces_filtered` that takes both `JoinFilter`
and the ablation flag). When disabled, skip tiers 2-4.

**Step 4: Run test — PASS**

**Step 5: Commit**

```bash
rtk git add src/calibrator.rs src/calibrate.rs
git commit -m "feat(calibrate): add disable_fuzzy_matching ablation flag"
```

---

### Task 7: JoinFilter for provenance-based corpus slicing

**Files:**
- Modify: `src/calibrate.rs`

**Step 1: Write failing tests**

```rust
#[test]
fn join_filter_by_version_excludes_other_versions() {
    let traces = vec![
        // trace with provenance.quorum_version = "0.18.4"
        // trace with provenance.quorum_version = "0.19.0"
    ];
    let feedback = vec![/* matching feedback */];
    let filter = JoinFilter { quorum_version: Some("0.19.0".into()), ..Default::default() };
    let (_, stats) = join_feedback_and_traces_filtered(&feedback, &traces, &filter, false);
    // Only the 0.19.0 trace should be considered
}

#[test]
fn join_filter_clean_only_excludes_dirty() {
    // trace with dirty=true, trace with dirty=false
    let filter = JoinFilter { clean_only: true, ..Default::default() };
    // Only clean trace should be considered
}

#[test]
fn join_filter_default_retains_legacy_traces() {
    // trace with no provenance (legacy)
    let filter = JoinFilter::default();
    // Legacy trace should still be included
}

#[test]
fn join_filter_positive_excludes_legacy_traces() {
    // trace with no provenance (legacy)
    let filter = JoinFilter { quorum_version: Some("0.19.0".into()), ..Default::default() };
    // Legacy trace should be excluded (can't match version filter)
}
```

**Step 2: Run tests — FAIL**

**Step 3: Implementation**

Add `JoinFilter` struct and a filtering function:

```rust
#[derive(Debug, Default)]
pub struct JoinFilter {
    pub quorum_version: Option<String>,
    pub clean_only: bool,
    pub repo: Option<String>,
    pub commit_sha: Option<String>,
    pub run_id: Option<String>,
}

fn trace_passes_filter(trace: &serde_json::Value, filter: &JoinFilter) -> bool {
    let prov = &trace["provenance"];
    
    if let Some(ref ver) = filter.quorum_version {
        match prov["quorum_version"].as_str() {
            Some(v) if v == ver => {},
            _ => return false,
        }
    }
    if filter.clean_only {
        match prov["dirty"].as_bool() {
            Some(false) => {},
            _ => return false, // dirty=true or missing provenance
        }
    }
    // ... similar for repo, commit_sha, run_id
    true
}
```

Apply filter at the top of `join_feedback_and_traces_filtered` before indexing traces.

**Step 4: Run tests — PASS**

**Step 5: Commit**

```bash
rtk git add src/calibrate.rs
git commit -m "feat(calibrate): add JoinFilter for provenance-based corpus slicing"
```

---

### Task 8: CLI flags for calibrate command

**Files:**
- Modify: `src/main.rs` (calibrate subcommand)

**Step 1: Write failing test**

```rust
// CLI integration test
#[test]
fn calibrate_accepts_filter_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_quorum"))
        .args(["calibrate", "--trace-version", "0.19.0", "--clean-only"])
        .output()
        .unwrap();
    // Should not fail with "unexpected argument"
    // (may fail for other reasons like no traces, but arg parsing should succeed)
}
```

**Step 2: Run test — FAIL (flags not recognized)**

**Step 3: Add CLI flags to the calibrate subcommand**

Wire `--trace-version`, `--clean-only`, `--trace-repo`, `--trace-commit`,
`--trace-run-id` flags. Pass to `JoinFilter` and into
`join_feedback_and_traces_filtered()`.

Also wire `--disable-fuzzy` flag that sets the ablation knob.

**Step 4: Run test — PASS**

**Step 5: Commit**

```bash
rtk git add src/main.rs
git commit -m "feat(cli): add --trace-version/--clean-only/--disable-fuzzy calibrate flags"
```

---

### Task 9: Update existing test fixtures

**Files:**
- Modify: `src/calibrator.rs` (test fixtures)
- Modify: `src/calibrator_fingerprint.rs` (if applicable)
- Modify: any other file constructing CalibratorTraceEntry

**Step 1: Run full test suite to find compilation errors**

Run: `rtk cargo test --lib 2>&1 | head -50`

**Step 2: Fix all CalibratorTraceEntry construction sites to include `provenance: None`**

**Step 3: Run full test suite**

Run: `rtk cargo test --bin quorum`
Expected: All tests pass

**Step 4: Commit**

```bash
rtk git add -A
git commit -m "fix(tests): add provenance field to all CalibratorTraceEntry fixtures"
```

---

### Task 10: Verification

**Step 1: Full test suite**

```bash
rtk cargo test --bin quorum
```

**Step 2: Clippy**

```bash
rtk cargo clippy
```

**Step 3: Release build**

```bash
rtk cargo build --release
```

**Step 4: Manual smoke test**

```bash
cargo run -- review src/calibrator_trace.rs 2>/dev/null | head -5
tail -1 ~/.quorum/calibrator_traces.jsonl | python3 -c "import sys,json; d=json.loads(sys.stdin.read()); print(json.dumps(d.get('provenance',{}), indent=2))"
```

Verify provenance has version, repo, commit_sha, dirty, review_model, run_id, timestamp.
