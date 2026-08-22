# Trace Provenance Metadata — Design

## Problem

Calibrator traces (`~/.quorum/calibrator_traces.jsonl`) carry per-finding
calibration decisions but lack provenance metadata. You cannot tell:

- Which quorum version generated a trace
- Which repo/commit the reviewed code came from
- Whether the working tree was dirty
- Which LLM model produced the finding
- Whether fuzzy matching was enabled

This blocks two capabilities:

1. **Corpus rebuild** — re-running reviews on historical code at known commits
   to produce apples-to-apples data when testing new calibrator features.
2. **Ablation analysis** — filtering traces by version/config to measure the
   impact of changes like fuzzy matching or AST scoping.

## Current State

`CalibratorTraceEntry` has 14 fields. The only provenance-adjacent fields are
`file_path` (added in PR3, 100% populated on new traces) and
`severity_change_reason` (Track B). No version, repo, commit, model, or
config information is recorded.

`ReviewRecord` (in `review_log.rs`) already tracks `quorum_version`, `repo`,
`model`, `invoked_from`, `run_id`, and `flags` — but this metadata lives in a
separate JSONL file (`~/.quorum/reviews.jsonl`) with no join key to traces.

`GitOps` trait (in `context/inject/stale.rs`) provides `head_sha()` and
`has_local_changes()` — the plumbing for commit SHA and dirty detection already
exists.

## Design

### New struct: `TraceProvenance`

A metadata struct attached to every `CalibratorTraceEntry`. All fields are
`Option` with `skip_serializing_if` for backward compatibility with existing
trace lines. Serialized as a nested `"provenance"` object for strict schema
evolution (GPT-5.4 review: `#[serde(flatten)]` silently swallows unknown keys,
risking undetected schema drift in a long-lived JSONL corpus).

```rust
// src/calibrator_trace.rs

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TraceProvenance {
    /// Quorum version that generated this trace (e.g. "0.19.0").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_version: Option<String>,

    /// Repository name (basename of git root, e.g. "quorum").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,

    /// HEAD commit SHA at review time.
    /// None when not in a git repo or git unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,

    /// True if `git status --porcelain` was non-empty at review time.
    /// None when not in a git repo or git unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,

    /// LLM model used for this review (e.g. "gpt-5.4").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,

    /// Review run ID (ULID). Join key to ReviewRecord in reviews.jsonl.
    /// Generated once in main.rs before per-file fanout, reused by both
    /// trace emission and ReviewRecord persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,

    /// UTC timestamp when this trace was generated.
    /// Enables time-range filtering for corpus rebuild without relying
    /// on JSONL line ordering (which breaks after merges/rebuilds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}
```

### CalibratorTraceEntry addition

```rust
pub struct CalibratorTraceEntry {
    // ... existing 14 fields ...

    /// Provenance metadata: version, repo, commit, model, run_id, timestamp.
    /// Nested object (not flattened) for strict schema evolution.
    /// `None` for backward-compat with pre-provenance trace lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<TraceProvenance>,
}
```

Using `Option<TraceProvenance>` with `skip_serializing_if`: old trace lines
(no `provenance` key) deserialize to `None`. New trace lines get the full
nested object. Schema-strict: unknown keys inside `provenance` cause
deserialization errors rather than silent swallowing.

### Data flow

```
main.rs (review command)
  |-- detect git info ONCE before per-file fanout:
  |     head_sha(), has_local_changes(), repo name
  |-- CARGO_PKG_VERSION
  |-- model from config
  |-- run_id (ULID, generated before review, shared with ReviewRecord)
  |-- timestamp (Utc::now() at trace construction time)
  |
  +-> CalibratorConfig gets new field:
        pub trace_provenance: Option<TraceProvenance>
        |
        +-> pipeline.rs::review_file()
              |
              +-> calibrator::calibrate(findings, feedback, config, file_path)
                    |  (config already carried -- NO new parameter)
                    |
                    +-> make_no_match_trace() / make_trace_entry()
                          read provenance from config, attach to each trace
```

**Key constraint (GPT-5.4 H3):** provenance MUST be computed once per
`quorum review` invocation in `main.rs`, before the per-file parallel fanout.
Placing it on `CalibratorConfig` (which is already threaded through) avoids
adding a new parameter to `calibrate()`, `calibrate_with_index()`,
`make_no_match_trace()`, or `make_trace_entry()` (GPT-5.4 H2).

Cost: one `git status --porcelain` + one `git rev-parse HEAD` per review run.
These are already executed for staleness detection in the context module.

### Fuzzy matching ablation config

Add to `CalibratorConfig`:

```rust
/// When true, ALL fuzzy matching tiers are disabled and only raw exact
/// matching is used. Binary toggle: "all fuzzy off" for A/B testing
/// fuzzy matching impact on corpus join rate. NOT per-tier granular.
pub disable_fuzzy_matching: Option<bool>,
```

Honored in `join_feedback_and_traces()`: when enabled, skip tiers 2-4
(normalized exact, fuzzy same-file, normalized title-only) and only use
tier 1 (raw exact) + raw title-only.

Env var: `QUORUM_DISABLE_FUZZY_MATCHING=1`.

### Join-time provenance filtering

`join_feedback_and_traces()` gains optional filters:

```rust
#[derive(Debug, Default)]
pub struct JoinFilter {
    /// Only include traces from this quorum version.
    pub quorum_version: Option<String>,
    /// Only include traces from clean commits (dirty == false).
    pub clean_only: bool,
    /// Only include traces from this repo.
    pub repo: Option<String>,
    /// Only include traces from this specific commit.
    pub commit_sha: Option<String>,
    /// Only include traces from this specific review run.
    pub run_id: Option<String>,
}
```

**Legacy trace semantics:** traces with `provenance: None` (pre-upgrade) are
retained when no filter is specified (`JoinFilter::default()`). Any positive
filter excludes legacy traces because their provenance fields are all `None`
and cannot match. This creates an effective "new corpus only" window until
enough post-upgrade traces accumulate — an intentional tradeoff documented
here so it doesn't surprise callers.

`run_calibrate` in `main.rs` exposes these as CLI flags:
`--trace-version`, `--clean-only`, `--trace-repo`, `--trace-commit`,
`--trace-run-id`.

### Backward compatibility

- All new fields are `Option` with `serde(default)`.
- `provenance: Option<TraceProvenance>` means old trace lines (no provenance
  key) deserialize to `None` cleanly.
- New trace lines get a nested `"provenance": {...}` object.
- Schema-strict: unknown keys inside `provenance` cause serde errors
  (unlike `flatten` which silently swallows them).
- Existing JSON processing: `jq .provenance` accesses the new metadata;
  `.finding_title` etc. remain at root level, unchanged.

### Graceful degradation

When git is unavailable (files outside a repo, git not installed, `.git`
missing), ALL git-derived provenance fields (`repo`, `commit_sha`, `dirty`)
degrade to `None`. `quorum_version`, `review_model`, `run_id`, and
`timestamp` are always populated. This matches the existing `GitOps` trait
shape where `head_sha()` returns `Option<String>`.

## What this does NOT include

- **Corpus rebuild CLI** — deferred until after AST scoping ships. The
  provenance metadata makes rebuild possible; the rebuild command itself
  is a separate workstream.
- **Category-aware canonical forms** — deferred. AST scoping may produce
  more consistent titles, reducing the need for template normalization.
- **Legacy trace backfill** — not worth the fragility. Legacy traces
  (5,817 entries, all pre-PR3) will age out via recency decay (~83d
  half-life). New traces already have 100% file_path coverage.
  Provenance-based analyses will effectively be "new corpus only" until
  sufficient post-upgrade traces accumulate.
- **Per-tier fuzzy ablation** — binary toggle only. Per-tier granularity
  is overengineering at this stage; document and revisit if needed.

## Files to modify

| File | Change |
|------|--------|
| `src/calibrator_trace.rs` | Add `TraceProvenance` struct, add `provenance: Option<TraceProvenance>` to `CalibratorTraceEntry` |
| `src/calibrator.rs` | Add `trace_provenance: Option<TraceProvenance>` and `disable_fuzzy_matching: Option<bool>` to `CalibratorConfig`; read provenance from config in trace factory helpers |
| `src/pipeline.rs` | Populate `calibrator_config.trace_provenance` from `PipelineConfig` before calibrator calls |
| `src/main.rs` | Compute `TraceProvenance` once from git info + version + model + run_id; set on `CalibratorConfig` |
| `src/calibrate.rs` | Add `JoinFilter` struct; honor `disable_fuzzy_matching` in join logic; apply provenance filters |
| Tests | Backward compat deserialize, provenance round-trip, schema-strict unknown key rejection, fuzzy ablation, join filtering with legacy traces |

## Test plan summary

1. `TraceProvenance` serialization: round-trip with all fields, round-trip with all `None`
2. Backward compat: old trace JSON (no `provenance` key) deserializes to `provenance: None`
3. Nested object shape: new traces have `"provenance": {...}` (not flattened top-level keys)
4. Schema strictness: extra unknown keys inside `provenance` object cause deserialization error
5. `make_no_match_trace` / `make_trace_entry` read provenance from `CalibratorConfig` correctly
6. `calibrate()` propagates provenance to all output traces without new function parameters
7. `join_feedback_and_traces` with `disable_fuzzy_matching=true` only uses raw exact + raw title-only
8. `JoinFilter` filters by version/repo/clean/commit/run_id; legacy traces excluded by positive filters
9. `JoinFilter::default()` retains legacy traces (no filter = include all)
10. Pipeline integration: provenance flows from `main.rs` through `CalibratorConfig` to written traces
11. Graceful degradation: non-git files produce provenance with `None` for repo/commit_sha/dirty

## Frontier model review

Reviewed by GPT-5.4 (2026-05-05). Key changes incorporated:

- H1: Switched from `#[serde(flatten)]` to nested `Option<TraceProvenance>` for schema safety
- H2: Provenance carried on `CalibratorConfig` instead of new function parameters
- H3: Explicit constraint that git info computed once in `main.rs` before per-file fanout
- M1: Renamed `model` to `review_model` to avoid field name collision
- M2: Added `commit_sha` and `run_id` to `JoinFilter` for exact corpus slicing
- M3: Documented binary fuzzy ablation as intentional scope limit
- M4: Added `timestamp` field to `TraceProvenance`
- M5: Documented legacy trace filter semantics (excluded by positive filters)
- L1: Documented graceful degradation for non-git environments
- L2: Added run_id ordering note (generated before review fanout)
