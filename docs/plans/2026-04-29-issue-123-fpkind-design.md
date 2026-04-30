# Issue #123 — FpKind enum + per-kind calibrator behavior (design, Layer 1)

**Date:** 2026-04-29
**Issue:** [#123](https://github.com/jsnyder/quorum/issues/123)
**Scope:** Layer 1 only — vocabulary + per-kind calibrator + CLI/MCP surface. Layers 2 (auto-stale + reference doctor) and 3 (LLM-driven migration) tracked separately.

## Problem

Today's `Calibrator::verdict_weight` treats every FP identically: 1.0× human weight, 120-day recency half-life. That equates a "the LLM hallucinated this regex" FP with "this is FP under our trust model assumption." The two rot at very different rates — the latter is overturned the moment the trust model evolves (2026-04-14 ast_grep symlink FPs that 2026-04-28 cross-model PAL contradicted are the load-bearing example). Without vocabulary to distinguish them, the corpus accumulates structural rot.

## Layer 1 scope (this PR)

1. `FpKind` enum + `FeedbackEntry::fp_kind: Option<FpKind>` field with serde back-compat.
2. Per-kind recency in `verdict_weight`.
3. Few-shot/calibrator precedent pool: exclude `OutOfScope`, surface `PatternOvergeneralization::discriminator_hint`.
4. CLI `--fp-kind` flag on `quorum feedback`.
5. MCP `fpKind` JSON field on the `feedback` tool.
6. Tests covering each variant's calibrator behavior + serde round-trip + back-compat.

**Deferred to Layer 2:** auto-stale on cross-model TP corroboration, `quorum feedback doctor` reference validation.
**Deferred to Layer 3:** `quorum feedback reclassify` LLM-driven migration over the 2302 existing entries.

## Schema

```rust
/// Discriminates the *reason* a finding was marked FP. Calibrator applies
/// different decay/scope rules per kind. Defaults via Option<FpKind> = None
/// mean "Hallucination semantics" — preserves behavior on the 2302 existing
/// entries (no migration required).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FpKind {
    /// LLM invented a defect that doesn't exist. Structural; never expires.
    Hallucination,

    /// Real defect, but a compensating control elsewhere prevents it. The
    /// reference is the load-bearing assumption — if the control is removed,
    /// the FP becomes stale.
    CompensatingControl { reference: String },

    /// FP under the current trust model only. Likely-rotting — calibrator
    /// applies a 40d half-life (vs. 120d default).
    TrustModelAssumption,

    /// Pattern fires correctly on similar code in different contexts; THIS
    /// instance is the exception. Calibrator should NOT learn to suppress
    /// the pattern; the discriminator hint goes into the few-shot prompt
    /// instead so the LLM can re-flag the pattern but distinguish.
    PatternOvergeneralization { discriminator_hint: Option<String> },

    /// Real defect, but tracked in another PR/issue. Not actually an FP —
    /// excluded from the precedent pool entirely.
    OutOfScope { tracked_in: Option<String> },
}
```

`FeedbackEntry` gains:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    // existing fields...
    pub provenance: Provenance,

    /// Discriminates Verdict::Fp entries by reason. None ↔ Hallucination
    /// semantics for back-compat with pre-bump rows. Meaningful only when
    /// `verdict == Fp`; ignored on Tp/Partial/Wontfix/ContextMisleading.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp_kind: Option<FpKind>,
}
```

`#[serde(skip_serializing_if = "Option::is_none")]` keeps new rows compact when no kind is specified. Deserialization of pre-bump rows yields `fp_kind: None` ↔ `Hallucination` behavior.

## Calibrator changes

Two surface areas: `verdict_weight` (recency by kind) and the precedent-selection filter (exclusion of `OutOfScope`, hint extraction for `PatternOvergeneralization`).

### Per-kind recency

`verdict_weight` takes `now: DateTime<Utc>` as a parameter rather than calling `chrono::Utc::now()` inline. This makes calibrator tests deterministic, kills wall-clock coupling, and lets mutation testing pin the half-life constants precisely. All existing call sites in `calibrator.rs` thread `Utc::now()` through at the entry point.

```rust
fn verdict_weight(entry: &FeedbackEntry, now: DateTime<Utc>) -> f64 {
    let provenance_weight = /* unchanged: PostFix=1.5, Human=1.0, External=0.7, etc. */;

    let half_life_days = match (&entry.verdict, &entry.fp_kind) {
        (Verdict::Fp, Some(FpKind::TrustModelAssumption)) => 40.0,
        // Hallucination, CompensatingControl, PatternOvergeneralization: default 120d.
        // OutOfScope shouldn't reach here (filtered upstream), but if it does, default.
        _ => 120.0,
    };
    let age_days = (now - entry.timestamp).num_days().unsigned_abs() as f64;
    let recency_weight = (-age_days / half_life_days).exp();

    provenance_weight * recency_weight
}
```

Rationale: 40d half-life means a `TrustModelAssumption` FP decays to ~50% weight in 40 days, ~25% in 80 days, ~6% in 160 days. Cross-model corroboration TPs accumulating over time can outweigh a stale trust-model FP without manual intervention. The 2026-04-14 example would have decayed to ~0.36× by 2026-04-28 (14d gap × 40d half-life), which is the right shape — still present but no longer dominant against fresh cross-model TP signal.

### `OutOfScope` exclusion

`calibrate()` and `calibrate_with_index()` currently filter out auto-calibrate entries when `!config.use_auto_feedback`. Add a parallel filter excluding `Verdict::Fp` entries with `fp_kind == Some(FpKind::OutOfScope { .. })` from the precedent pool entirely. These entries don't represent "this finding is wrong" — they represent "this finding is real, just tracked elsewhere." Including them in the FP pool would suppress legitimate findings.

### `PatternOvergeneralization` discriminator hint

When few-shot injection picks a precedent, today's render uses `entry.finding_title` + `entry.reason`. Add a third element when `fp_kind == Some(FpKind::PatternOvergeneralization { discriminator_hint: Some(hint) })`: surface the hint as `Why FP: {reason}\nWhen the pattern IS a real bug: {hint}`. This keeps the LLM from blanket-suppressing the pattern while still learning the discriminator.

For now: do NOT change the calibrator's suppression weight for PatternOvergeneralization. Treat it as a normal FP (counts toward suppress weight), but inject the hint at few-shot time. Layer 2/3 may revisit if the discriminator-injection alone is insufficient.

## CLI surface

```bash
quorum feedback \
  --file src/ast_grep.rs \
  --finding "Symlink in rules dir" \
  --verdict fp \
  --fp-kind trust-model \
  --reason "Single-user dev machine threat model"

quorum feedback \
  --file src/llm_client.rs \
  --finding "..." \
  --verdict fp \
  --fp-kind compensating-control \
  --fp-reference "PR #99 input validator at line 42" \
  --reason "..."

quorum feedback \
  --file src/foo.rs \
  --finding "..." \
  --verdict fp \
  --fp-kind pattern-overgeneralization \
  --fp-discriminator "When `var` is in a #[derive] expansion, ignore" \
  --reason "..."

quorum feedback \
  --file src/foo.rs \
  --finding "..." \
  --verdict fp \
  --fp-kind out-of-scope \
  --fp-tracked-in "#456" \
  --reason "..."
```

`--fp-kind` accepts: `hallucination` (default if omitted with `--verdict fp`), `trust-model`, `compensating-control`, `pattern-overgeneralization`, `out-of-scope`. The kind-specific suffix flags (`--fp-reference`, `--fp-discriminator`, `--fp-tracked-in`) provide the associated data; missing required suffix on a kind that needs one is an error.

`--fp-kind` is ignored when `--verdict` is not `fp` (warn, don't fail — composability with shell pipelines).

## MCP surface

`feedback` tool gains a `fpKind` field with JSON-typed variants matching the enum's serde representation:

```json
{ "verdict": "fp", "fpKind": "hallucination" }
{ "verdict": "fp", "fpKind": "trust_model_assumption" }
{ "verdict": "fp", "fpKind": { "compensating_control": { "reference": "PR #99" } } }
{ "verdict": "fp", "fpKind": { "pattern_overgeneralization": { "discriminator_hint": "..." } } }
{ "verdict": "fp", "fpKind": { "out_of_scope": { "tracked_in": "#456" } } }
```

Validation: `fpKind` ignored when `verdict != "fp"`. Required associated data missing → MCP error.

## Migration

Zero-touch for the existing 2302 entries: `Option<FpKind>` defaults to `None` via serde, calibrator falls through to 120d default for `None`. Existing tests pass unchanged.

The reclassification of existing rows (assigning `Hallucination`/`TrustModelAssumption`/etc. to old entries based on their `reason` text) is Layer 3 — out of scope here.

## Test plan

All calibrator tests pin `now` explicitly via the `verdict_weight(entry, now)` parameter. No test depends on wall-clock; mutation-test-friendly tolerances throughout.

| ID | Test | Asserts |
|---|---|---|
| T1 | `fp_kind_hallucination_default_recency` | A 120-day-old `Hallucination` weight ≈ 0.368 (1.0 × e^-1); pinned tolerance `(0.366..=0.370)`. |
| T2 | `fp_kind_trust_model_decays_3x_faster` | A 40-day-old `TrustModelAssumption` weight ≈ a 120-day-old `Hallucination` weight (both at e^-1 ≈ 0.368). **Plus second assertion**: `trust_w < halluc_at_40d` proving the 40d branch fired (kills "both arms collapsed to same value" mutant). |
| T3 | `fp_kind_out_of_scope_excluded_from_precedent_pool` | Calibrate w/ 3 OutOfScope FPs against a matching finding → finding survives. **Plus positive control**: same body with `fp_kind: Some(Hallucination)` → finding suppressed. Without the control, a mutant disabling suppression entirely passes. Explicit `Finding` field initialization (no `Finding::default()` mystery guest). |
| T4 | `fp_kind_pattern_overgeneralization_discriminator_in_few_shot` | (a) Hint present → renders into prompt with marker phrase; (b) hint = `None` → no marker phrase; (c) hint > 200 chars → truncated with ellipsis. Routed through `build_review_prompt` if `render_precedent_for_few_shot` is private. |
| T5 | `fp_kind_serde_back_compat` | Pre-bump JSON row (no `fp_kind` key) deserializes to `fp_kind: None`; re-serialized JSON omits the `fp_kind` key (skip-serializing-if-none). Re-deserialized struct equals original (no silent data loss). |
| T6 | `fp_kind_serde_round_trip_each_variant` | Each of the 5 variants (with associated data permutations) serializes and deserializes to itself. |
| T7 | `cli_fp_kind_flag_parses_each_variant` | `--fp-kind trust-model` etc. produce the right enum. Pinned error path: `compensating-control` without `--fp-reference` → `into_fp_kind()` returns `Err` (clap doesn't validate cross-field). Invalid value (`--fp-kind bogus`) → clap rejects. `--fp-kind` on `--verdict tp` → `tracing::warn`, doesn't fail, fp_kind dropped. |
| T8 | `mcp_fp_kind_field_each_variant` | MCP tool accepts each variant, **persists to a tmpdir-backed feedback store**, then reads back and asserts `entry.fp_kind == expected`. Helper `handler_with_writable_feedback_store(tmpdir: &Path)` pinned. Malformed payload (`compensating_control` missing `reference`) → MCP error. |
| T9 | `fp_kind_compensating_control_keeps_120d_recency` + `fp_kind_none_routes_to_default_branch` | (a) 120-day-old `CompensatingControl` weight ≈ 0.368. (b) **Negative anchor**: `verdict_weight(None@40d) > verdict_weight(TrustModel@40d)` proving `None` routes to the default 120d arm, not coincidentally to the trust-model 40d arm. |
| T10 | `fp_kind_ignored_on_non_fp_verdict` | Regression-lock (passes by construction given the match shape in T3): setting `fp_kind` on a `Tp` entry uses the 120d default branch (1.0 × e^(-40/120) ≈ 0.717). Documented as regression lock, not RED→GREEN ritual. |

T1, T2, T9 cover the recency change. T3 is the OutOfScope exclusion (with control). T4 is the discriminator surfacing (with absence + truncation). T5+T6 lock the schema. T7+T8 lock the surfaces (T8 with tmpdir read-back, not liar `is_ok()`). T10 locks the verdict-coupling.

## Risks

| Risk | Mitigation |
|---|---|
| Existing 2302 entries shift behavior | Zero by construction: `Option<FpKind> = None` ↔ Hallucination semantics. T5 locks this. |
| 40d half-life too aggressive — kills useful trust-model precedents prematurely | Bias toward humility: 40d gives ~6% weight at 160d, still nonzero. Cross-model TP corroboration is the intended balancer. Adjustable in Layer 2 if metrics show otherwise. |
| `OutOfScope` exclusion silently drops findings the user expected to see suppressed | OutOfScope is for "this finding is REAL, just tracked elsewhere" — exactly the case where suppression is wrong. The CLI doc string should be explicit. |
| `OutOfScope` recorded with `tracked_in: None` orphans the deferral — user forgets to file the follow-up issue, finding stays noisy | CLI + MCP emit a `tracing::warn` when `OutOfScope` is recorded without `tracked_in`. Don't fail (composability), but make the omission visible. |
| `PatternOvergeneralization` discriminator hint inflates few-shot prompt size | Hint is optional; cap at ~200 chars when rendered (truncate with ellipsis). Few-shot already truncates long fields. |
| Future Layer 2/3 work depends on Layer 1 not being arbitrarily reshaped | Lock the enum's serde representation in T5+T6. |

## Definition of done

- [ ] `cargo test --bin quorum` passes (existing + 10 new tests).
- [ ] `cargo clippy` clean.
- [ ] `cargo build --release` clean.
- [ ] All 5 variants serde-round-trip.
- [ ] Existing 2302 entries in `~/.quorum/feedback.jsonl` continue to deserialize unchanged.
- [ ] CLI + MCP both accept all 5 variants.
- [ ] CLI + MCP emit `tracing::warn` when `OutOfScope` recorded without `tracked_in`.
- [ ] `verdict_weight` takes `now: DateTime<Utc>` parameter; all call sites updated.
- [ ] Adoption telemetry: `fp_kind_utilization_rate` counter on `TelemetryEntry` (fraction of `Verdict::Fp` records carrying a kind in current session) — informs Layer 3 prioritization.
- [ ] CHANGELOG entry under "Feedback".

## Files touched

- `src/feedback.rs` — `FpKind` enum + `FeedbackEntry::fp_kind` field, `ExternalVerdictInput::fp_kind`.
- `src/calibrator.rs` — per-kind recency in `verdict_weight`, `OutOfScope` filter in `calibrate`/`calibrate_with_index`.
- `src/review.rs` (or wherever few-shot rendering lives) — discriminator hint surfacing.
- `src/main.rs` — CLI `--fp-kind` flag + suffix flags.
- `src/mcp/tools.rs` + `src/mcp/handler.rs` — MCP `fpKind` field.
- `CHANGELOG.md` — Feedback entry.

## Out of scope (Layers 2 + 3)

- Auto-stale flagging on cross-model TP corroboration.
- `quorum feedback doctor` reference validation for `CompensatingControl`.
- `quorum feedback reclassify` LLM-driven migration over existing 2302 entries.
- Per-kind suppression-weight changes (other than recency). Layer 1 leaves suppression logic alone.
