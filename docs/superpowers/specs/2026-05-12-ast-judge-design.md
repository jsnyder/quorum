# AST Finding Judge and Rule Growth

**Date:** 2026-05-12
**Issues:** #11 (rule_id tracing), #17 (stats --by-rule), #281 (semantic grounding)
**Branch:** TBD

## Problem

AST findings get hardcoded confidence (0.95 for LocalAst, 0.90 for Linter) and skip grounding. This limits the rule set to high-precision patterns only. 51% of TP findings (852 in the feedback corpus) come from LLM-only review with no AST rule equivalent. Many of those patterns are syntactically matchable but need semantic validation to avoid false positives.

There is no `rule_id` on Finding, so per-rule precision is unmeasurable. `Source::Linter` carries the tool name ("ast-grep") but not the individual rule ID.

## Design

Two phases. Phase 1 ships independently and provides the observability foundation. Phase 2 adds the judge and speculative rules.

---

## Phase 1: Rule Identity and Per-Rule Stats

### 1.1 Finding struct changes

Add to `Finding` in `src/finding.rs`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub rule_id: Option<String>,
```

Format convention:

| Source | rule_id example | Derivation |
|--------|----------------|------------|
| ast-grep | `ast-grep:python/bare-except-pass` | `ast-grep:{lang}/{rule.id}` |
| LocalAst | `local-ast:complexity` | `local-ast:{pattern-name}` |
| LocalAst | `local-ast:python/eval-exec` | `local-ast:{lang}/{pattern-name}` |
| Linter (future) | `ruff:E501` | `{linter}:{rule-code}` |
| LLM | `None` | LLM findings have no rule |

Backward compat: `serde(default)` means legacy JSON without the field deserializes as `None`.

### 1.2 Populate rule_id at construction sites

**ast_grep.rs:277** -- the rule id and language are both available in the scan loop:
```rust
rule_id: Some(format!("ast-grep:{}/{}", lang_str, rule.id)),
```

Where `lang_str` is derived from the `SupportLang` loop variable (e.g., "python", "typescript").

**analysis.rs** -- each `Finding` construction (~30 sites) gets a descriptive rule_id. Pattern naming:
- `local-ast:complexity` for `analyze_complexity`
- `local-ast:{lang}/{pattern}` for language-specific patterns in `scan_insecure_*`, e.g., `local-ast:python/eval-exec`, `local-ast:rust/unwrap-in-non-test`

**LLM findings** -- remain `rule_id: None`.

### 1.3 Feedback linkage

Add `rule_id: Option<String>` to `FeedbackEntry` in the feedback store. When recording a verdict against a finding that has a `rule_id`, copy it through. Same `serde(default, skip_serializing_if)` pattern for backward compat with existing `feedback.jsonl` rows.

### 1.4 Stats dimensions

New CLI subcommands:
- `quorum stats --by-rule` -- per-rule TP/FP/partial breakdown with precision rate
- `quorum stats --by-source` -- aggregated by source kind (local-ast, ast-grep, llm)

Same MIN_SAMPLE gate, table UI, and sparkline rendering as existing `--by-repo` / `--by-caller`. Support glob filtering: `quorum stats --by-rule --rule "ast-grep:python/*"`.

---

## Phase 2: LLM Micro-Judge

### 2.1 Rule metadata schema

Add optional `metadata` block to ast-grep YAML rules. ast-grep ignores unknown top-level keys; quorum parses it separately.

```yaml
id: broad-exception-catch
language: Python
severity: warning
message: "Catching broad Exception may mask errors"
rule:
  pattern: "except Exception"
metadata:
  precision: speculative    # high | medium | speculative
  judge: required           # required | optional | skip
```

Defaults for all existing rules: `precision: high`, `judge: skip`. No YAML changes needed for the existing 53 rules.

Parsed in `ast_grep.rs` into:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleMetadata {
    pub precision: PrecisionTier,
    pub judge: JudgeRequirement,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrecisionTier {
    #[default]
    High,
    Medium,
    Speculative,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JudgeRequirement {
    Required,
    Optional,
    #[default]
    Skip,
}
```

Built-in patterns in `analysis.rs` define their metadata in code. All existing patterns: `precision: High, judge: Skip`.

### 2.2 Pipeline placement

New stage between AST collection and merge in `pipeline.rs`. Runs in parallel with the LLM review call (they are independent):

```
Parse -> Hydrate -> Local AST -> ast-grep -> [JUDGE] -> Merge -> Grounding -> Calibrate -> Output
                                              [LLM] ----^
```

The judge and LLM reviewer run concurrently. Both feed into `merge_findings`.

### 2.3 Judge protocol

Batched per-file: one LLM call per file containing all findings that need judging. The source code is sent once.

**Prompt structure:**
- Redacted source code (same as LLM reviewer gets)
- For each finding: rule_id, title, line range, matched evidence snippet

**Expected response (structured JSON):**
```json
[
  {
    "rule_id": "ast-grep:python/broad-exception-catch",
    "verdict": "tp",
    "confidence": 0.85,
    "reason": "This catches Exception at a low-level utility, not a top-level handler"
  },
  {
    "rule_id": "ast-grep:python/missing-await",
    "verdict": "fp",
    "confidence": 0.92,
    "reason": "Return value is passed to asyncio.create_task, await not needed"
  }
]
```

Verdicts: `tp` (keep, use judge confidence), `fp` (floor confidence at 0.15), `uncertain` (keep with reduced confidence).

### 2.4 Gating logic

```
for each AST/ast-grep finding:
  match metadata.judge:
    Skip     -> pass through unchanged (existing behavior)
    Required -> must pass judge to enter merge; if judge unavailable, hold at metadata baseline
    Optional -> judge if available, pass through if judge offline
```

Findings rejected by the judge (`fp` verdict) are not dropped. They get:
- `confidence` floored at 0.15
- `calibrator_action: Some(Disputed)`
- Ranked very low in output

This avoids silently suppressing true positives.

### 2.5 Confidence model changes

Add to `Finding`:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub judge_confidence: Option<f32>,

#[serde(default, skip_serializing_if = "Option::is_none")]
pub precision_tier: Option<PrecisionTier>,
```

Updated `compute_confidence`:

```rust
let base = match (&self.source, &self.precision_tier) {
    (Source::LocalAst, Some(PrecisionTier::High)) | (Source::LocalAst, None) => 0.95,
    (Source::LocalAst, Some(PrecisionTier::Medium))                          => 0.80,
    (Source::LocalAst, Some(PrecisionTier::Speculative))                     => 0.50,
    (Source::Linter(_), Some(PrecisionTier::High)) | (Source::Linter(_), None) => 0.90,
    (Source::Linter(_), Some(PrecisionTier::Medium))                          => 0.75,
    (Source::Linter(_), Some(PrecisionTier::Speculative))                     => 0.45,
    (Source::Llm(_), _) => self.grounding_confidence
        .filter(|c| c.is_finite())
        .map(|c| c.clamp(0.0, 1.0))
        .unwrap_or(0.5),
};

// Judge overrides base (not multiplies)
let base = self.judge_confidence
    .filter(|c| c.is_finite())
    .map(|c| c.clamp(0.0, 1.0))
    .unwrap_or(base);

// Then apply calibrator and agreement factors as before
```

Existing high-precision rules behave identically (precision_tier defaults to None, judge_confidence stays None).

### 2.6 Cost control

- Only speculative/medium rules with `judge: required|optional` hit the judge
- High-precision rules bypass entirely (zero additional cost for existing rules)
- Judge uses a configurable model (default: fast/cheap model)
- If zero findings need judging for a file, no judge call is made
- Judge timeout: 5 seconds per file, fail-open (findings pass through unjudged with metadata baseline confidence)
- `QUORUM_JUDGE_MODEL` env var for model selection

### 2.7 Feature flag

- `--judge` CLI flag or `QUORUM_JUDGE=1` env var to enable
- Off by default until validated on the pilot rule set
- When disabled, speculative rules still fire but use their metadata baseline confidence (no LLM call)

### 2.8 Telemetry

New counters on `TelemetryEntry`:

| Counter | Description |
|---------|-------------|
| `judge_calls` | Number of LLM judge calls made |
| `judge_approved` | Findings approved (tp verdict) |
| `judge_rejected` | Findings rejected (fp verdict) |
| `judge_uncertain` | Findings marked uncertain |
| `judge_skipped` | Findings that bypassed judge (high precision) |
| `judge_timeout` | Calls that timed out (fail-open) |

All counters use `serde(default)` for backward compat.

---

## Phase 2 Pilot: Speculative Rules

Initial set of 10 rules marked `precision: speculative, judge: required`. Drawn from `docs/plans/LOWER_PRECISION_RULES.md` and `docs/feedback-pattern-mining.md` gap analysis.

| # | Language | Rule ID | Source Doc | TP Cluster | Why Judge Needed |
|---|----------|---------|------------|------------|-----------------|
| 1 | Python | `broad-exception-catch` | LOWER_PRECISION_RULES | async_issue, missing_validation | Intentional at top-level handlers |
| 2 | Python | `subprocess-no-check` | LOWER_PRECISION_RULES | missing_validation | May check returncode manually |
| 3 | Python | `re-compile-in-loop` | LOWER_PRECISION_RULES | regex_issue (21 TPs) | Loop variable may be dynamic |
| 4 | Python | `missing-await` | gap analysis | async_issue (34 TPs) | Fire-and-forget / task spawning |
| 5 | Python | `logging-debug-leak` | gap analysis | logging_debug (30 TPs) | Debug logging sometimes intentional |
| 6 | TypeScript | `nullish-coalescing-broad` | LOWER_PRECISION_RULES | null_undef (21 TPs) | `\|\|` is idiomatic for some falsy cases |
| 7 | Rust | `string-byte-slice-broad` | LOWER_PRECISION_RULES | -- | ASCII-only slices are safe |
| 8 | Rust | `discarded-result` | gap analysis | missing_validation | Intentional cleanup paths |
| 9 | TypeScript | `string-format-sql` | gap analysis | type_safety (18 TPs) | May use query builder |
| 10 | YAML | `jinja-loop-variable-scoping` | LOWER_PRECISION_RULES | yaml_issue (38 TPs) | Regex-based detection is fragile |

Rules 6 and 7 are broader variants of existing high-precision rules (`nullish-coalescing-preferred`, `string-byte-slice`). They widen the match pattern and rely on the judge to filter.

### Graduation criteria

- >80% judge TP rate after 2-3 weeks -> promote to `precision: medium, judge: optional`
- 50-80% -> keep as speculative, tune pattern
- <50% -> retire or rewrite the rule
- Measured via `quorum stats --by-rule`

---

## Files Modified

### Phase 1
- `src/finding.rs` -- add `rule_id` field, update `compute_confidence`
- `src/ast_grep.rs` -- populate `rule_id` from `rule.id` + language
- `src/analysis.rs` -- populate `rule_id` at ~30 Finding construction sites
- `src/feedback.rs` -- add `rule_id` to `FeedbackEntry` (line 91)
- `src/dimensions.rs` -- add `group_by_rule` and `group_by_source` dimension functions
- `src/main.rs` -- wire new CLI flags

### Phase 2
- `src/ast_grep.rs` -- parse `metadata` block, add `RuleMetadata`/`PrecisionTier`/`JudgeRequirement` types
- `src/finding.rs` -- add `judge_confidence`, `precision_tier` fields, update `compute_confidence`
- `src/pipeline.rs` -- insert judge stage between AST collection and merge
- `src/judge.rs` -- new module: judge trait, LLM judge implementation, batching, timeout
- `src/telemetry.rs` -- add judge counters
- `src/main.rs` -- `--judge` flag, `QUORUM_JUDGE_MODEL` env var
- `rules/python/` -- add 5 new speculative rule YAMLs
- `rules/typescript/` -- add 2 new speculative rule YAMLs
- `rules/rust/` -- add 2 new speculative rule YAMLs
- `rules/yaml/` -- add 1 new speculative rule YAML

## Non-Goals

- Replacing the LLM reviewer with AST rules -- the judge validates AST findings, it does not replace the full LLM review
- Scope-tree semantic grounding (#281) -- complementary approach that can layer in later as a zero-cost optimization for patterns where tree-sitter provides enough context
- Automated rule generation -- future work; this design provides the measurement infrastructure
- Judge for LLM findings -- LLM findings already have grounding; the judge is for AST findings only
