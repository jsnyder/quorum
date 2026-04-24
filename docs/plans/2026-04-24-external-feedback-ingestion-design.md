# External feedback ingestion — design

**Issue:** [#32 — feedback: ingest verdicts from external review agents](https://github.com/jsnyder/quorum/issues/32)
**Status:** Design approved — ready for implementation plan
**Date:** 2026-04-24
**Reviewers consulted:** gpt-5.4, gemini-3-pro-preview

## Problem

Other review agents (pal, third-opinion, gemini, reviewdog, future ensemble members) produce verdicts on quorum's findings that currently have nowhere to go. Quorum's calibrator already supports weighted provenance tiers (`Human` 1.0x, `PostFix` 1.5x, `AutoCalibrate` 0.5x) but has no channel for cross-agent verdicts. We need a fourth tier so cross-agent agreement accelerates calibration without requiring human triage.

## Design commitments

1. **Cross-model precedent, not self-verification.** External verdicts come from a *different* model, so they avoid the self-reinforcing failure mode that sank `AutoCalibrate` in v0.11.0. They are trusted more than auto-calibrate, less than human.
2. **External verdicts are first-class, uncapped precedent.** Multiple concordant external verdicts can independently soft- or full-suppress a finding without human corroboration. This is intentional.
3. **All three ingestion surfaces ship in v1.** Inbox (file drop), CLI flag, and MCP tool — so any agent integration pattern works on day one.
4. **No dedup, no per-agent weight table in v1.** Both are explicit non-goals in issue #32.

## Data model

### New `Provenance` variant (`src/feedback.rs`)

```rust
pub enum Provenance {
    Human,
    PostFix,
    AutoCalibrate(String),
    External {
        agent: String,              // normalized: lowercase, trimmed
        model: Option<String>,      // agent's LLM model (e.g. "gemini-3-pro-preview")
        confidence: Option<f32>,    // clamped [0,1]; stored but IGNORED in v1 calibration
    },
    Unknown,
}
```

**Serde representation** (externally tagged, matches existing `ContextMisleading` style):

```json
{"external": {"agent": "pal", "model": "gemini-3-pro-preview", "confidence": 0.9}}
```

Legacy JSONL rows without a `provenance` field continue to deserialize as `Unknown` via existing `#[serde(default)]`. Round-trip tests extend to cover the new variant.

### Agent-name hygiene

Normalize at every ingestion boundary (inbox parse, CLI parse, MCP parse): `agent.trim().to_lowercase()`. Empty string after normalization → reject with error. No registry, no validation beyond that; per-agent reliability scoring is an explicit non-goal.

### `confidence` is diagnostic-only in v1

The field is stored in the JSONL (future-compat) but the calibrator ignores it. We may weight by confidence in a follow-up after we have data on how honest agent-supplied confidences are. Document this in the field docstring so no one assumes it affects weight.

## Calibrator

### Weight (`src/calibrator.rs:46`)

```rust
let provenance_weight = match &entry.provenance {
    Provenance::PostFix => 1.5,
    Provenance::Human => 1.0,
    Provenance::External { .. } => 0.7,    // NEW
    Provenance::AutoCalibrate(_) => 0.5,
    Provenance::Unknown => 0.3,             // preserved — was already explicit
};
```

**0.7 is a deliberate middle value.** Rationale:
- Cleanly separates from the self-verification failure mode (`AutoCalibrate = 0.5`).
- Below `Human = 1.0` — external reviewers have less context and no PR ownership.
- Enough that two fresh external verdicts can accumulate to a material signal.
- Not so high that a single external verdict dominates.

### Filter treatment

External verdicts **fall through** the `use_auto_feedback` filters at calibrator.rs:75 and :337 (i.e., they are never filtered out). They also land in the **uncapped `other_*_weight` bucket** at calibrator.rs:130/142/367/380, not the `.min(1.0)` cap applied to `AutoCalibrate`.

**Threshold consequences** (under existing defaults):
- 2 external FPs (≈1.4 weighted) → meets `soft_fp_weight >= 1.0` → soft-suppress to Info severity.
- 3 external FPs (≈2.1 weighted) → meets `full_suppress_weight >= 1.5` → full suppression possible.
- Similarity + recency decay will typically reduce effective weight below these nominal values.

This is the most consequential commitment in the design. If it turns out to be too aggressive in practice, the first knob to tune is the 0.7 weight — not the filter path.

## Ingestion paths

### Shared input DTO (`src/feedback.rs`)

```rust
pub struct ExternalVerdictInput {
    pub file_path: String,
    pub finding_title: String,
    pub finding_category: Option<String>,  // defaults to "unknown" at ingest
    pub verdict: Verdict,
    pub reason: String,
    pub agent: String,                     // normalized lowercase/trimmed
    pub agent_model: Option<String>,
    pub confidence: Option<f32>,           // clamped to [0,1]; None if out-of-range
}
```

Single constructor `FeedbackStore::record_external(input) → Result<()>` is used by all three ingestion paths. It:
- sets `provenance = External { agent, model: agent_model, confidence }`
- sets `timestamp = Utc::now()`
- sets `FeedbackEntry.model = None` (that field is the *reviewer* model; the external agent's model lives in the provenance struct)
- defaults missing `finding_category` to `"unknown"`
- rejects empty `agent` after normalization

Mirrors the existing `record_context_misleading` typed-constructor pattern.

### Path 1: Inbox drain

**Layout:**
- Agents drop JSONL files into `~/.quorum/inbox/`.
- One `ExternalVerdictInput` JSON per line.
- Quorum drains on next `quorum review` or `quorum stats` invocation.

**Function signature:**
```rust
pub fn drain_inbox(
    inbox_dir: &Path,
    processed_dir: &Path,
) -> Result<DrainReport>;

pub struct DrainReport {
    pub drained_files: usize,
    pub entries: usize,
    pub errors: Vec<DrainError>,
    pub processed_bytes: u64,   // total size of files moved to processed/
}
```

**Behavior:**
1. **Fast path:** if `read_dir(inbox_dir).next().is_none()`, return zero-work report. No further IO.
2. For each `*.jsonl` in inbox:
   - Read and parse line-by-line. Malformed lines → `errors`, skip (mirrors existing `load_all` leniency at feedback.rs:90).
   - For each valid line: call `record_external`.
   - On success: atomically `rename` file to `<processed>/<original>.<ulid>.jsonl`.
     - ULID suffix prevents collisions on duplicate filenames.
     - Swallow `ENOENT` on rename (lock-free multi-process race: another `quorum` process got it first; move on).
3. After drain: sum bytes in `processed_dir`. If > 50MB, emit one-shot `tracing::warn!` suggesting manual cleanup. Never auto-delete.

**Invocation site:** `src/main.rs`, before dispatching to `pipeline::run_review` or `run_stats`. Pipeline stays IO-pure.

**Why no delete-on-success?** The raw inbox files have retraining value; user policy is to preserve archives. 50MB warning threshold is generous given per-verdict payload size.

### Path 2: CLI flag extension

Extend `cli::FeedbackOpts` with:
```
--from-agent <name>              triggers external provenance
--agent-model <model>            optional — the external agent's LLM
--confidence <float>             optional — clamped [0,1]
```

`--from-agent` conflicts with `--provenance` via clap `ArgGroup` — passing both is a hard error ("use --from-agent for external, --provenance for internal"). Silent overrides breed ambiguity.

When `--from-agent` is present: `run_feedback` routes through `record_external` instead of the default Human path.

### Path 3: MCP tool

Extend `FeedbackTool` schema (`src/mcp/tools.rs`) with optional:
```rust
pub from_agent: Option<String>,
pub agent_model: Option<String>,
pub confidence: Option<f32>,
```

Handler at `src/mcp/handler.rs::handle_feedback`:
- `if from_agent.is_some()` → route through `record_external`.
- Else → preserve existing Human path byte-for-byte (backward-compat).

Two code paths gated on `from_agent.is_some()` is idiomatic additive evolution; do **not** try to unify by injecting a synthetic `agent = "human"` (that would pollute provenance weights).

## Stats surfacing

Extend existing feedback-tier aggregation in `src/analytics.rs` and `run_stats` table output:

```
FEEDBACK (tier breakdown)
  Human      : 1,382  (tp 68% / fp 24% / partial 6% / wontfix 2%)
  PostFix    :    51  (tp 88% / fp  8% / partial 4% / wontfix 0%)
  External   :   207  (tp 71% / fp 22% / partial 5% / wontfix 2%)      ← NEW
    top agents: pal (142), third-opinion (43), gemini (22)
  AutoCalib  :     0
  Unknown    :    37
```

One new row + agent sub-line. No new tables. Sample-size gate (`MIN_SAMPLE=5`) applies per-agent to avoid noisy one-off entries in the sub-line.

## Test strategy

**Policy-locking tests** (written BEFORE implementation, must fail on current main):
1. `external_variant_roundtrips_through_jsonl`
2. `external_weight_is_0_7`
3. `unknown_weight_is_0_3_preserved`
4. `two_external_fps_soft_suppress`
5. `three_external_fps_full_suppress`
6. `external_not_filtered_when_use_auto_feedback_false`
7. `external_not_capped_like_auto_calibrate` (2 × 0.7 > 1.0 cap)

**Ingestion tests:**
8. `drain_inbox_empty_returns_zero_work`
9. `drain_inbox_valid_file_appends_and_moves`
10. `drain_inbox_malformed_line_skipped_rest_drained`
11. `drain_inbox_handles_enoent_race_gracefully`
12. `cli_from_agent_writes_external_provenance`
13. `cli_from_agent_conflicts_with_provenance`
14. `mcp_from_agent_writes_external_provenance`
15. `mcp_feedback_without_from_agent_still_human`
16. `agent_name_normalized_lowercase_trimmed`
17. `confidence_clamped_to_unit_interval`

**Stats test:**
18. `stats_external_tier_shows_count_and_top_agents`

## Out of scope (v1.1+)

- `quorum feedback --prune-processed --older-than 30d` — manual cleanup subcommand
- Per-agent reliability scoring (explicit non-goal in issue #32)
- Automated adversarial-agent detection
- Confidence-weighted calibration (revisit after collecting data)
- Real-time streaming ingestion
- Direct pal / third-opinion integrations (those live in the respective tools; quorum just provides the inbox contract)

## Open questions for implementation

None blocking. The design is settled. If the 0.7 weight proves too aggressive in real-world use, it's a one-line tuning change with a calibrator test update.

## References

- Issue: https://github.com/jsnyder/quorum/issues/32
- MEMORY.md "Calibrator Analysis (2026-04-16)" — background on why AutoCalibrate was disabled
- MEMORY.md "v0.9.2 Changes" — existing few-shot / calibrator weighting design
- `src/feedback.rs` — existing provenance enum, verdict types
- `src/calibrator.rs:46-60` — existing `verdict_weight` function (extension point)
- `src/calibrator.rs:75, :337` — `use_auto_feedback` filter sites
- `src/calibrator.rs:123-145, :360-383` — `auto_*_weight` cap sites
