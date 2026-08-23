# PR3 Threshold Calibration — Test Strategy & Gap Analysis

## Acceptance Criteria by Task

**T1 (metrics.rs):** PR curve produces correct (P, R, T) triples for 4-sample dataset; ties collapse; empty/all-neg/all-pos/NaN inputs handled. **AC: 6 tests pass, no panics on degenerate input.**

**T2 (threshold selection):** `threshold_at_precision` returns lowest qualifying threshold (max recall); returns None when unachievable. `f1_optimal_threshold` picks max-F1 point. **AC: 4 tests pass.**

**T3 (file_path on trace):** New field round-trips through serde; old trace lines without field deserialize with `None`. **AC: 2 tests pass, zero backward-compat breakage.**

**T4 (threshold_config.rs):** TOML round-trip preserves values; partial configs (only boost/only suppress) parse; malformed TOML returns Err; missing file returns None. **AC: 4 tests pass.**

**T5 (calibrator wiring):** Data-driven suppress/boost override legacy; legacy unchanged when thresholds absent; `force_threshold` overrides config. **AC: 4 tests pass, all existing calibrator tests still pass.**

**T6 (calibrate subcommand logic):** Join produces labeled scores; wontfix excluded; class balance gates enforced; suppress threshold is low cutoff; ordering enforced; duplicates skipped. **AC: 7 tests pass.**

**T7 (CLI wiring):** `quorum calibrate` and `quorum calibrate --dry-run` parse. **AC: 2 tests pass.**

**T8 (startup load):** Thresholds from TOML populate CalibratorConfig at review time. **AC: 1 test pass.**

**T9 (verification):** Full suite green, clippy clean, `--dry-run` produces sane output on real corpus.

---

## Missing Tests (Gaps in the Plan)

### 1. Score boundary at exactly the threshold
The plan tests `score < suppress_threshold` and `score >= boost_threshold` but never tests `score == threshold`. Add:
- `score == suppress_threshold` should NOT suppress (strict `<`)
- `score == boost_threshold` should boost (inclusive `>=`)

### 2. Zero-total-weight edge (tp=0, fp=0)
`score = tp / (tp + fp)` divides by zero when both are 0.0. The plan's fallback `else { 0.5 }` needs a dedicated test. Confirm 0.5 is the right neutral — it sits in the dead zone between suppress and boost, which is correct.

### 3. Partial verdict mapping
`partial` maps to positive (TP-like). No test verifies this. Add one `partial` entry to the join test to confirm it lands as `is_positive = true`.

### 4. `context_misleading` verdict in join
The plan skips wontfix. It should also skip `context_misleading` (which exists in the feedback store). Verify the `_ => continue` catch-all handles it — add an explicit test entry with this verdict.

### 5. Negative infinity / +inf scores
`is_finite()` filters NaN but does it filter +/-inf? `f64::INFINITY.is_finite()` is false, so yes. Add a test with `f64::INFINITY` score to confirm it is filtered alongside NaN.

### 6. TOML write atomicity
If `quorum calibrate` crashes mid-write, a partial `calibrator_thresholds.toml` could corrupt fallback. The plan does not mention atomic write (write to tmp + rename). Not a test gap per se, but a risk. Recommend write-to-temp-then-rename in Task 7 implementation.

### 7. `--dry-run` does NOT write file
No test verifies dry-run leaves the filesystem untouched. Add a tempdir integration test: run calibrate with `--dry-run`, assert no TOML file created.

### 8. Suppress + boost interaction on same finding
What happens when both thresholds are set but a finding's score falls in the dead zone (above suppress, below boost)? Should be no-op (legacy confirm logic). No test covers this gap zone explicitly.

### 9. Recency decay interaction with threshold
Threshold is calibrated on current-corpus weights. But at runtime, weight decays over time. A finding that was above `boost_threshold` during calibration may drop below it at runtime as precedents age. This is by-design but warrants a comment + one test showing that a decayed weight changes the decision.

### 10. `file_path` propagation through both calibrator paths
Task 3 adds `file_path` to the trace struct but the plan only mentions `make_trace_entry`. Verify `make_no_match_trace` also gets `file_path`. Currently it does not — it creates a trace with no file path. Either add it or explicitly test that no-match traces have `file_path: None`.

---

## Edge Cases That Could Bite

1. **Join key normalization**: Feedback `file_path` may be absolute (`/home/user/src/db.rs`) while trace `file_path` is relative (`src/db.rs`). The plan joins on exact string match. Mismatched paths silently produce zero joins and zero samples. Add path normalization or document the constraint.

2. **Tie-breaking in `threshold_at_precision`**: When multiple consecutive curve points share the same precision, `.last()` picks the lowest threshold. But if the curve has precision oscillations (possible with tied scores), the "lowest" may not be the rightmost on the recall axis. The current implementation is correct because the curve is sorted descending, but add a regression test with an oscillating-precision scenario.

3. **Large corpus performance**: `join_feedback_and_traces` builds a HashMap. Fine for thousands of entries, but the plan does not test with empty traces file (distinct from empty samples after join). Add: feedback exists but traces file is empty or missing.

4. **SeverityChangeReason for data-driven paths**: The plan reuses `Disputed` for suppress and `Boosted` for boost. Should it introduce new variants like `DataDrivenSuppress` / `DataDrivenBoost` for observability? Not blocking, but worth a TODO.

---

## Integration Test Suggestions

### E2E: calibrate + review round-trip (tempdir-isolated)
```
1. Create tempdir as $HOME
2. Write synthetic feedback.jsonl (20+ entries, mixed TP/FP)
3. Write synthetic calibrator_traces.jsonl (matching keys + file_path)
4. Run `quorum calibrate` (not --dry-run)
5. Assert calibrator_thresholds.toml exists with valid [suppress] and [boost]
6. Run `quorum review <file>` with tracing
7. Assert calibrator_traces.jsonl entries show data-driven thresholds applied
   (check trace action matches expected suppress/boost for known scores)
```

### E2E: legacy fallback when TOML absent
```
1. Create tempdir as $HOME with feedback.jsonl but NO calibrator_thresholds.toml
2. Run `quorum review <file>` 
3. Assert behavior matches legacy (compare trace entries against known-good baseline)
```

### E2E: force_threshold override
```
1. Write calibrator_thresholds.toml with suppress=0.3, boost=0.7
2. Set QUORUM_FORCE_THRESHOLD=0.5
3. Run review, assert force value is used (finding with score 0.4 suppressed,
   score 0.6 not boosted — force_threshold replaces both)
```
