# Test Plan — #97 External cap + #100 feedback parent dir

**Scope:** Two unrelated bugfixes shipped together.
- `src/calibrator.rs::calibrate` — cap External-provenance contribution to weight buckets.
- `src/feedback.rs::FeedbackStore::record` — auto-create parent dir before opening file.

## Design pre-conditions for testability

- Promote the cap to a module constant: `pub(crate) const EXTERNAL_WEIGHT_CAP: f64 = 1.4;` and reference it in tests rather than hardcoding `1.4`. Cap value can then change without test churn.
- Apply the cap in **both** weight-computation blocks (similar~v1 around L88-151 and similarity-scaled~v2 around L365-389). Each must add `external_*_weight` parallel to `auto_*_weight` and combine: `tp_weight = auto.min(1.0) + external.min(EXTERNAL_WEIGHT_CAP) + humanish` (Human + PostFix + Unknown go in `humanish`, uncapped).
- Tests live next to existing `mod tests` blocks; reuse `fb()` helper and add `fb_external(title, cat, verdict, age_days)` builder.

## 1. Test cases

### #97 — calibrator External cap

| # | Name | Setup | Assertion | Rationale |
|---|------|-------|-----------|-----------|
| 1 | `external_flood_does_not_overwhelm_human_fp` | 1 finding "SQL injection". Feedback: 100 External TP + 1 Human FP, all matching title/cat. | `result.findings.len() == 0` AND `result.suppressed == 1`. | Behavioral outcome — does not couple to the constant. Survives any cap value `<= human_fp_weight ≈ 1.0`. This is the headline regression. |
| 2 | `external_tp_bucket_capped_at_constant` | 1 finding. Feedback: 10 External TP entries (timestamps = `Utc::now()`, so recency≈1.0). No other entries. Build via path that produces a single `CalibratorTraceEntry`. | `trace.tp_weight` is approximately `EXTERNAL_WEIGHT_CAP` (within `1e-6`), NOT `10 * 0.7 = 7.0`. | Direct verification the cap is applied. References the constant so cap-value changes propagate. |
| 3 | `external_fp_bucket_capped_at_constant` | Mirror of #2 but verdict=Fp. | `trace.fp_weight ≈ EXTERNAL_WEIGHT_CAP`. | Symmetry — both TP and FP buckets must cap. |
| 4 | `external_below_cap_passes_through_unchanged` | 1 External TP fresh. | `assert!((trace.tp_weight - 0.7).abs() < 1e-3)` (not `EXTERNAL_WEIGHT_CAP`). | `.min()` must not floor — single entries stay at their natural weight. Kills a `.min` → `.max` mutant (would force weight up to 1.4). |
| 5 | `human_and_postfix_remain_uncapped` | 5 Human TP + 5 PostFix TP, fresh. | `trace.tp_weight > EXTERNAL_WEIGHT_CAP + 1.0` (i.e. ≈ 5\*1.0 + 5\*1.5 = 12.5). | Cap must NOT leak into the humanish bucket. |
| 6 | `external_global_across_agents` | 50 External TP from agent="pal" + 50 External TP from agent="third-opinion". | `trace.tp_weight ≈ EXTERNAL_WEIGHT_CAP`. | Per #97 spec: cap is global across agents, not per-agent. |
| 7 | `external_cap_applies_in_similarity_scaled_block` | Same as #2 but invoked via the embedding-similar code path (L360+). | Same assertion as #2. | Both code blocks must be patched — easy to fix one and forget the other. |

### #100 — feedback record parent dir

| # | Name | Setup | Assertion | Rationale |
|---|------|-------|-----------|-----------|
| 8 | `record_creates_missing_parent_directory` | `tempdir / "missing/nested/feedback.jsonl"` (parent does not exist). | `record(&entry).is_ok()`; file exists; `load_all().len() == 1`. | Headline regression. |
| 9 | `record_works_when_parent_exists` | `tempdir / "feedback.jsonl"` (parent = tempdir, exists). | `record(&entry).is_ok()`; round-trip via `load_all`. | No regression in happy path. |
| 10 | `record_appends_without_truncating` | Pre-create file with 1 entry, parent exists. Call `record` again. | `load_all().len() == 2`. | Guards the `OpenOptions::append(true)` contract — `create_dir_all` must not switch mode to truncate/create-new. |
| 11 | `record_returns_err_on_unwritable_parent` | Set parent to a path that cannot be created (e.g. existing **file** where parent dir should go). | `record(&entry).is_err()`. Walk error chain via `err.chain()` and assert at least one link is an `io::Error`. Do NOT match on error message strings. | Confirms error path propagates; decoupled from message wording. |

## 2. Edge cases

- **Recency decay on cap (age>0):** External entry with `timestamp = now - 365 days` should produce `verdict_weight ≈ 0.7 * exp(-365/120) ≈ 0.033`. 100 such entries → `~3.3` raw → capped to `EXTERNAL_WEIGHT_CAP`. Already covered behaviorally by #1 with age=0; add one test where 2 stale Externals (raw sum ≈ `0.07`) should pass through uncapped (verifies cap is `min`, not `clamp`).
- **Mixed provenance:** 1 PostFix TP (1.5) + 1 Human TP (1.0) + 50 External TP. Expect `tp_weight ≈ 1.5 + 1.0 + EXTERNAL_WEIGHT_CAP = 3.9`. Asserts each bucket sums correctly without cross-contamination.
- **Empty External (no regression):** 3 Human FP, 0 External. Assert `trace.fp_weight` within `[2.95, 3.05]` (3 × Human × recency ≈ 1.0) — numeric range, no snapshots, no clock coupling.
- **Pre-existing parent dir for #100:** covered by test #9 above (explicit no-regression).
- **Path with no parent component**: dropped — `std::fs::create_dir_all("")` behavior is stdlib-owned and `Path::parent()` returns `None` for `"feedback.jsonl"` (guard already `if let Some(parent)`). Not a real failure mode for our code.

## 3. What NOT to test

- Do NOT assert the literal value `1.4` — reference `EXTERNAL_WEIGHT_CAP`.
- Do NOT re-test `verdict_weight` provenance multipliers (already covered).
- Do NOT test recency half-life math — covered by `verdict_weight_future_dated_entry_is_not_max_weight`.
- Do NOT test inbox-drain, agent-name normalization, or confidence clamping — owned by `record_external` tests.
- Do NOT test `OpenOptions` flag semantics for `record` — stdlib contract.
- Do NOT test full `calibrate` suppression thresholds for non-External cases — existing tests cover.
- Do NOT add CLI/MCP integration tests; these are pure unit fixes.

## 4. Fixture strategy

**External FeedbackEntry builder** (add to calibrator `tests` module):
```rust
fn fb_external(title: &str, cat: &str, verdict: Verdict, age_days: i64) -> FeedbackEntry {
    FeedbackEntry {
        file_path: "test.rs".into(),
        finding_title: title.into(),
        finding_category: cat.into(),
        verdict,
        reason: "ext".into(),
        model: None,
        timestamp: Utc::now() - chrono::Duration::days(age_days),
        provenance: crate::feedback::Provenance::External {
            agent: "pal".into(),
            model: None,
            confidence: None,
        },
    }
}
```

**Determinism without stubbing `Utc::now`:** `verdict_weight` reads `Utc::now()` directly. Rather than introduce a clock trait (out of scope), use **age-based assertions with tolerances**:
- For "fresh" entries, set `timestamp = Utc::now()` and assert weights within `±1e-3` (floor of single-call drift).
- For decayed-entry tests, assert ratios/orderings (e.g. `w_stale < w_fresh * 0.5`), not absolute values.
- For the headline flood test (#1), behavioral assertion (`suppressed == 1`) is clock-independent.

**Tempdir for #100:** use existing `tempfile::TempDir` pattern from `feedback.rs::tests::test_store()`. New helper `test_store_with_missing_parent()` returns `(FeedbackStore, TempDir)` where the store path is `tempdir.path().join("a/b/c/feedback.jsonl")`.

**Trace inspection:** tests #2-7 need `CalibratorTraceEntry`. Build a single finding, single similar-precedent set, then read `result.traces[0].tp_weight` / `fp_weight`. Existing tests already do this pattern — just extend.
