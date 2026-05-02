# Calibrator Precision + Architectural Cleanup Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan PR-by-PR.

**Goal:** Improve quorum's finding precision and verifiability by adding deterministic hallucination detection, calibrated tunable thresholds, and structured finding metadata — while removing accumulated dead code and centralizing scattered logic.

**Architecture:** Each finding carries `reasoning`, `confidence`, `cited_lines`, and a strict-enum `category`. Post-LLM, an AST grounding pass verifies cited symbols exist; ungrounded findings are demoted+flagged (not deleted, per design discussion — detection is the high-value step, action is product-shaped). The calibrator splits into two independent paths (boost-TP, suppress-FP), each with thresholds calibrated to a stated precision target via offline PR-curve analysis on the 1,740-label feedback corpus. Active-learning prompts surface low-confidence findings for annotation. Existing duplicate code paths (`calibrate` / `calibrate_with_index`, scattered trace emission, pipeline calibrator invocation) collapse into shared helpers; `auto_calibrate.rs` (dead since v0.11.0) and `fp_suppress_count` (unused config field) are deleted.

**Tech Stack:** Rust 2024, MSRV 1.88, tree-sitter 0.26, ast-grep 0.42, fastembed bge-small-en-v1.5, sklearn (offline calibration script). No new runtime dependencies expected for the Rust binary; the calibration script is Python-side tooling.

**Architectural principles for this plan:**
1. **Ablate-to-decide.** Every new component ships with an ablation knob. Existing components without knobs get one added. Before declaring a component "done," run the 4-arm harness to verify it pulls weight; if not, delete it.
2. **Demote, don't delete.** When grounding fails or precedents disagree, prefer demote+flag over outright suppression. The user can decide.
3. **Cleanup folded into feature work.** Each PR's surface area cleans up the area it touches. No "cleanup later" PRs.
4. **Schema changes are forward-only.** Use `#[serde(default, skip_serializing_if = "Option::is_none")]` for new optional fields so old trace/feedback files keep parsing.

---

## Research-Validated Foundations

This plan rests on findings from `/tmp/quorum-exp-189/research_fp_hallucination.md` (2026-05-01):
- **AST grounding** for hallucinations: 100% precision, 87.6% recall (arXiv:2601.19106).
- **Citation-grounded prompting** prevented 100% of invalid-citation hallucinations on GPT-4-class models (arXiv:2512.12117).
- **Separate TP-boost vs FP-suppress paths** is Semgrep Assistant's most-cited architectural decision (96% reviewer agreement).
- **Calibrate threshold to precision target** via `sklearn.TunedThresholdClassifierCV` is the deployable form of conformal prediction (Fisch ICML 2022).
- **Active-learning uncertainty sampling** (Settles 2009) targets `|tp_score - fp_score|` near 0 for highest annotation ROI.

The research also confirmed quorum is **ahead of published academic practice** on the embedding-similarity + recency-weighted feedback calibrator axis — no paper documents a deployed system at this scale. We are not behind, we have a different shape of problem.

---

## Cleanup Targets Surfaced by Architecture Survey

Source: `Explore` agent survey of `src/calibrator.rs`, `feedback.rs`, `pipeline.rs`, `auto_calibrate.rs`, `finding.rs`, `calibrator_trace.rs`.

**P1 (load-bearing, do early):**
- DELETE `src/auto_calibrate.rs` (~200 lines, disabled in v0.11.0; references in `analytics.rs`, `stats.rs` to audit).
- DELETE `CalibratorConfig.fp_suppress_count` (no call sites).
- EXTRACT `calibrate_core_decision()` helper. `calibrate` (line 150) and `calibrate_with_index` (line 455) share ~270 lines of preamble: disabled-check, clock injection, precedent filtering, OutOfScope exclusion, empty-corpus NoMatch trace.

**P2 (clean as we go):**
- EXTRACT trace factory fn from 11 scattered `CalibratorTraceEntry` builds in `calibrator.rs` (lines 200, 243, 328, 396, 477, 551, 633, 701, ...).
- EXTRACT pipeline `invoke_calibration()` helper from duplicated branches (lines 634-654 vs 979-998).
- AUDIT FpKind enum-vs-string consistency across CLI/MCP/inbox/feedback_store.

**P3 (next refactor pass):**
- EXTRACT `boost_severity()` constants (CRITICAL_KEYWORDS, HIGH_FREE_CATEGORIES) into a `SeverityBoostDecision` struct with public surface for testing.
- ADD parity test: `calibrate()` and `calibrate_with_index()` produce identical output on the same input.

---

## PR Sequence

### PR 1: Foundation — Schema + Dead-Code Deletion + Helper Extraction

**Branch:** `feat/calibrator-foundation`

**Files:**
- Create: `src/category.rs` (new `Category` enum + `From<&str>` shim)
- Modify: `src/finding.rs` — add `reasoning`, `confidence`, `cited_lines`, switch `category: String` → `category: Category` (with `#[serde(default)]` and shim)
- Modify: `src/calibrator.rs` — extract `calibrate_core_decision()`; extract `trace_entry()` factory
- Modify: `src/calibrator_trace.rs` — add factory function
- Delete: `src/auto_calibrate.rs`
- Delete: `fp_suppress_count` field from `CalibratorConfig`
- Modify: `src/analytics.rs`, `src/stats.rs` — remove auto_calibrate references
- Modify: prompt template — instruct LLM to emit `reasoning`, `confidence` (0-1), `cited_lines` (line range it claims supports the finding)
- Test: `tests/finding_schema.rs`, `tests/calibrator_parity.rs`

**Test-first targets:**
1. `Category` enum round-trips through serde for every variant.
2. `Category::from("bug")` and `Category::from("code_quality")` map deterministically (these are the worst-precision categories — get folded into a known target like `Maintainability`).
3. Old feedback.jsonl rows with free-text `category` deserialize via `From<String>` shim without data loss.
4. `calibrate(findings, &config)` and `calibrate_with_index(findings, &mut empty_index, &config)` produce identical traces on empty index.
5. After extracting `trace_entry()` factory, all 11 emission sites still produce byte-identical JSONL output to a regression snapshot.
6. Old `auto_calibrate.rs`-touching tests deleted; no orphan references in `analytics.rs` / `stats.rs`.

**Success criteria:**
- `cargo test --bin quorum` passes (was 1746, after this PR ~1755 with new schema + parity tests).
- `cargo clippy` clean.
- Diff stat: net negative line count (cleanup > additions).
- LLM still emits well-formed findings on a sample of 5 dub-flow files; new fields populated.
- `~/.quorum/feedback.jsonl` and `~/.quorum/calibrator_traces.jsonl` from before this PR still parse without error.

**Ablation hooks:** None new (foundational).

**Cleanup folded in:** auto_calibrate.rs deletion, fp_suppress_count deletion, calibrate/calibrate_with_index dedup, trace factory.

---

### PR 2: AST Symbol-Existence Grounding Check

**Branch:** `feat/ast-grounding`
**Depends on:** PR 1 (uses `cited_lines` field)

**Files:**
- Create: `src/grounding.rs` — `verify_finding_grounding(finding, source) -> GroundingResult`
- Modify: `src/finding.rs` — add `grounding_status: Option<GroundingStatus>` (Verified | SymbolNotFound | LineOutOfRange | NotChecked)
- Modify: `src/pipeline.rs` — invoke grounding pass after LLM emit, before calibrator
- Modify: `src/feedback.rs` — auto-set `FpKind::Hallucination` on grounding-failed findings if user records FP verdict
- Test: `tests/grounding_test.rs`

**Algorithm (per arXiv:2601.19106):**
1. Parse `cited_lines` from finding.
2. Re-fetch `[line_start - 5, line_end + 5]` from source (already in memory during pipeline).
3. Extract identifier(s) from finding title using existing AST infrastructure (`tree-sitter-rust`, `tree-sitter-python`, etc.) — prefer backtick-wrapped or quoted symbols.
4. Run tree-sitter query over the fetched range; check identifier presence.
5. If absent → `GroundingStatus::SymbolNotFound`. Demote severity one step. Prefix description with `[symbol not found at cited location]`.
6. Findings without quoted identifiers in title → `GroundingStatus::NotChecked`.

**Test-first targets:**
1. Synthetic finding citing line 47 with symbol `parse_unified_diff`, source has `parse_unified_diff` at line 47 → `Verified`.
2. Same finding, source has `parse_diff` (typo) → `SymbolNotFound`, severity demoted, description prefixed.
3. Cited line range past EOF → `LineOutOfRange`, severity demoted.
4. Finding title without quoted identifier → `NotChecked`, severity unchanged.
5. Multi-byte UTF-8 file (issue #175 fixed in v0.18.2): grounding doesn't panic on emoji/CJK files.

**Success criteria:**
- All 5 test cases pass.
- Run on the 8-file `eval_v2.py` corpus: false-suppression rate (Verified findings that the user marks TP afterwards) under 5% on hand-graded sample.
- Run on a synthetic hallucination corpus (mutate cited symbols in known-good findings): detection rate >70%.

**Ablation knob:** `QUORUM_DISABLE_AST_GROUNDING=1`

**Cleanup folded in:** Centralize tree-sitter parser caching (currently each module instantiates its own).

---

### PR 3: PR-Curve Threshold Calibration

**Branch:** `feat/calibrated-thresholds`
**Depends on:** PR 1 (cleanup), PR 2 (grounding signal feeds into score)

**Files:**
- Create: `tools/calibrate_threshold.py` — offline PR curve script
- Create: `~/.quorum/calibrator_thresholds.toml` (generated; gitignored example committed)
- Modify: `src/calibrator.rs` — read thresholds from config, fall back to current hardcoded values if absent
- Modify: `src/main.rs` — add `quorum calibrate` subcommand that re-runs the script
- Modify: prompt — `confidence` field from PR1 feeds into the fused score
- Test: `tests/calibrator_threshold_config.rs`

**Algorithm:**
1. Load `~/.quorum/feedback.jsonl` → `(score, label)` pairs.
2. Score = α·precedent_vote + β·llm_confidence + γ·grounding_pass (binary). Initially α=1.0, β=0.0, γ=0.0; tune later.
3. Run `sklearn.metrics.precision_recall_curve`.
4. Pick thresholds at P=0.90, 0.95, 0.99.
5. Write to `~/.quorum/calibrator_thresholds.toml`:
   ```toml
   [boost]
   precision_target = 0.85
   threshold = 0.42
   [suppress]
   precision_target = 0.95
   threshold = 0.78
   ```
6. Calibrator reads these at startup; users edit precision_target, run `quorum calibrate` to regenerate threshold.

**Test-first targets:**
1. Synthetic dataset (50 TPs at score 0.2, 50 FPs at score 0.8) → threshold at P=0.95 lands between them.
2. Empty feedback corpus → fall back to current hardcoded thresholds.
3. Config file with malformed TOML → log warning, use defaults.
4. `quorum calibrate` subcommand regenerates the file.

**Success criteria:**
- Thresholds picked from real corpus; documented in CHANGELOG.
- Default `precision_target = 0.95` for suppress, `0.85` for boost (matches Semgrep's asymmetry).
- Old hardcoded thresholds removed from `calibrator.rs`.

**Ablation knob:** `QUORUM_FORCE_THRESHOLD=<float>` overrides config (for sweeps).

**Cleanup folded in:** Remove hardcoded `1.5` and `2.0` magic numbers from `calibrator.rs`.

---

### PR 4: Split TP-Boost vs FP-Suppress Paths

**Branch:** `refactor/split-tp-fp-paths`
**Depends on:** PR 1, PR 3 (independent thresholds)

**Files:**
- Modify: `src/calibrator.rs` — split single decision branch into `evaluate_boost()` and `evaluate_suppress()`
- Create: `src/calibrator/boost.rs` (extract `SeverityBoostDecision` struct with public CRITICAL_KEYWORDS, HIGH_FREE_CATEGORIES)
- Modify: `tests/calibrator_*.rs` — split boost tests from suppress tests
- Test: parity test that confirms split produces identical output to pre-split when thresholds match

**Architectural insight (Semgrep):** Boost and suppress are not symmetric. Over-boosting is annoying; over-suppressing is dangerous. The paths should be calibrated to different precision targets (boost at P=0.85, suppress at P=0.95) and have independent ablation knobs.

**Test-first targets:**
1. Pre-split snapshot test: capture all calibrator outputs on test corpus.
2. Post-split: same inputs produce same outputs with default thresholds.
3. With `QUORUM_DISABLE_BOOST=1`, boosts are skipped but suppressions still happen.
4. With `QUORUM_DISABLE_SUPPRESS=1`, suppressions are skipped but boosts still happen.

**Success criteria:**
- All existing calibrator tests pass.
- Two independent ablation knobs work.
- `boost.rs` constants are public for testing.

**Ablation knobs:** `QUORUM_DISABLE_BOOST=1`, `QUORUM_DISABLE_SUPPRESS=1`

**Cleanup folded in:** Extract magic constants from `boost_severity()` body into typed struct.

---

### PR 5: Active-Learning Annotation Prompts

**Branch:** `feat/active-learning-prompts`
**Depends on:** PR 1 (confidence field)

**Files:**
- Modify: `src/calibrator.rs` — flag findings where `|tp_weight - fp_weight| < 0.5 && total_weight < 2.0`
- Modify: `src/main.rs` (review output) — emit annotation request block for up to 3 flagged findings
- Test: `tests/active_learning.rs`

**Output format:**
```
[ANNOTATION REQUEST] 3 findings have low calibrator confidence.
Your feedback significantly improves future reviews:

  quorum feedback --file src/auth.rs --finding-id 01HX... --verdict <tp|fp> [--fp-kind hallucination|...]

  finding 1: ...
  finding 2: ...
  finding 3: ...
```

**Test-first targets:**
1. Synthetic 5-finding corpus with varying confidences → top 3 uncertain ones surfaced.
2. All findings high-confidence → no annotation block emitted.
3. `--no-annotation-prompts` flag suppresses the block.

**Success criteria:**
- Annotation prompts visible in non-JSON output.
- JSON output unchanged (machine-parseable consumers don't see them).
- Capped at 3 per session.

**Ablation knob:** `QUORUM_DISABLE_ACTIVE_LEARNING=1` or `--no-annotation-prompts`

---

### PR 6: Corpus Retag (One-Off Tooling)

**Branch:** `tooling/corpus-retag`
**Depends on:** PR 1 (Category enum locked)

**Files:**
- Create: `tools/retag_corpus.py` — LLM-as-Judge retagging script
- Create: `~/.quorum/feedback.jsonl.bak.YYYY-MM-DD` (backup before retag)

**Algorithm:**
1. Load `~/.quorum/feedback.jsonl`. Filter to entries with empty/missing `finding_category` (316 expected) and entries with FP verdict but no `fp_kind` (400 expected).
2. For each, send to LLM (gpt-5.4 via LiteLLM) with prompt:
   ```
   Given this finding's title, description, and verdict, classify into:
   - category: one of [security, correctness, logic, concurrency, reliability, robustness, error-handling, validation, performance, maintainability]
   - fp_kind (if verdict=fp): one of [hallucination, pattern_overgeneralization, trust_model_assumption, compensating_control, out_of_scope]
   Output JSON. Confidence 0-1. If confidence < 0.85, output category=null.
   ```
3. Hand-validate a 50-row stratified sample before applying.
4. Apply with confidence floor 0.85.
5. Re-run PR-curve script (PR3) on the retagged corpus to see calibration shift.

**Test-first targets:**
1. Run on a synthetic 20-row sample where ground truth is known → ≥85% accuracy.
2. Backup file created before mutating feedback.jsonl.
3. Atomic apply (temp file + rename, not in-place edit).

**Success criteria:**
- Hand-validation: ≥90% of judge mappings correct on 50-row sample.
- Post-retag corpus: <10% untagged FPs.
- PR-curve from PR3 re-run shows tighter precision/recall separation.

**Ablation knob:** `--dry-run` flag prints what would change without applying.

---

## Cross-Cutting: Component Effect Visibility (Continuous + Periodic)

The goal: make "is this component still pulling weight?" answerable continuously, not just during ad-hoc ablation sprints. Two complementary mechanisms.

### Continuous: Shadow-logging via traces

Every component records what it would have done both with and without firing, in the existing trace files. Track B already does this for the calibrator (`severity_change_reason` distinguishes Boosted vs BoostBlockedByGate vs Disputed vs BoostWeightTooLow vs NoMatch). Each subsequent PR extends the pattern:

| PR | New trace field | Captures |
|---|---|---|
| PR2 | `grounding_status` | Verified \| SymbolNotFound \| LineOutOfRange \| NotChecked, plus the original severity if grounding caused a demote |
| PR3 | `threshold_decision` | which precision-target threshold fired (boost vs suppress), score, threshold value |
| PR4 | `boost_path_taken` / `suppress_path_taken` | which split path evaluated this finding |
| PR5 | `surfaced_for_annotation` | bool — was this finding surfaced to the user as low-confidence? |

Cost: trivial. The data is already produced; we just record it.

### Continuous surface: `quorum stats --components`

**Branch:** `feat/stats-components` (post PR1-5; let's call it PR 5b)
**Files:**
- Modify: `src/stats.rs` — new `--components` view aggregates trace fields
- Modify: `src/main.rs` — add subcommand flag

**Output shape:**
```
=== Component effect (last 100 reviews) ===
component              | fires | flips | trend
rubric_gate_critical   |   42  |   12  | ↓ (was 18 last release)
rubric_gate_high       |   89  |    7  | →
calibrator_boost       |   34  |   34  | →
calibrator_suppress    |   18  |   18  | ↑
ast_grounding          |   47  |    9  | ↑ (new)
fewshot_retrieval      |  100  |  n/a  | (cost only — see --tokens)
```

`fires` = how many findings the component evaluated. `flips` = how many it changed (severity bump, demote, suppress, flag). Trend compares to the previous release's stats snapshot (stored in `~/.quorum/stats_snapshots/`).

**Auto-flag rule:** Components whose `flips/fires` ratio trends toward zero over 3+ release snapshots emit `candidate for removal` in the trend column. That's the trigger to run periodic ablation (below) for confirmation.

### Periodic: full ablation matrix

**Files:**
- Move: `/tmp/quorum-exp-189/eval_v2.py` → `tools/ablation/eval.py` for permanence
- Add: `tools/ablation/run_release_ablation.sh` — reproducible harness invocation per release

**Arms to measure:**
- `high_allowlist_off`: `QUORUM_HIGH_ALLOWLIST=off` (new knob in PR4) — does the HIGH category gate pull weight, or could rubric keywords cover both CRITICAL and HIGH?
- `single_tau`: `QUORUM_FPKIND_DECAY=single` (new knob) — does per-kind τ beat a single 83d τ?
- `fewshot_off + calibrator_off`: control arm; baseline LLM only.
- `grounding_off`: `QUORUM_DISABLE_AST_GROUNDING=1` — does grounding actually catch hallucinations on the eval corpus?
- `boost_off`, `suppress_off`: independent path ablations from PR4.

**Cadence:** Per-release, or whenever a stats trend triggers it. Reports archived under `docs/ablation/<version>.md`.

**Decision protocol:**
1. Stats `--components` shows trend toward zero over 3 releases.
2. Periodic ablation confirms: removing the component changes <X findings on the eval corpus (X = 1-2σ of LLM run-to-run variance).
3. File issue with the data; remove in follow-up release.

This formalizes "ablate-and-decide" as routine ops, not a special exercise.

---

## Out of Scope (Explicit)

- **Multi-agent specialized prompts** (cubic 51% FP reduction claim). Higher-leverage research findings recommend the path-split first; specialized agents are a downstream architectural change.
- **Symbolic-AI / dataflow taint** (Snyk DeepCode approach). Architectural pivot. Quorum's strength is the feedback-loop calibrator.
- **Full Chain-of-Verification critic** (~2x LLM cost per file). Targeted CRITICAL critic deferred until PR1-5 are measured.
- **NLI claim-level entailment.** Academic, no production deployment found.
- **Issue #122 supersession schema.** Different design surface; left as separate work.
- **Issue #188 sub-day clock skew on `verdict_weight`.** Independent bug; address separately.

---

## Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|------------|------------|
| Schema migration breaks existing feedback corpus | Low | `#[serde(default)]` on all new fields; `From<String>` shim on `Category`; round-trip test. |
| LLM doesn't comply with reasoning-first schema | Medium | Test on 20-file sample before flipping for everyone; retry-with-correction loop. |
| AST grounding produces false-suppressions on legitimate findings | Medium | Demote, don't delete (per design). Run on hand-graded corpus before merge. |
| LLM-as-Judge retag introduces new errors | Medium | Confidence floor 0.85; hand-validate 50 rows; atomic apply with backup. |
| PR-curve thresholds don't generalize across repos | Medium | Re-calibrate when corpus grows >200 entries; document in CHANGELOG. |
| Helper extraction breaks subtle behavioral parity | Medium | Snapshot regression test before refactor; parity test for `calibrate` vs `calibrate_with_index`. |

---

## Success Definition

**Per-PR:**
- All `cargo test --bin quorum` tests pass.
- `cargo clippy` clean.
- No new `#[allow(dead_code)]` introduced.
- Diff stat shows cleanup folded in (or net-neutral; never net-positive without a reason).

**End-to-end (after PR 1-5):**
- LLM-emitted findings carry `reasoning`, `confidence`, `cited_lines`, `category: Category`.
- Ungrounded findings auto-flagged.
- FP-suppress threshold tunable via `precision_target`.
- TP-boost and FP-suppress paths independently calibrated and ablatable.
- Active-learning prompts surface the 3 most uncertain findings per session.
- Ablation harness measures every component's effect; components that don't pull weight queued for removal.

**End-to-end (after PR 6):**
- Untagged-category FPs < 5% of corpus.
- Untagged-fp_kind FPs < 20% of corpus.
- PR-curve re-run shows tighter separation (target: P=0.95 threshold suppresses ≥30% of FPs at <5% TP cost).

---

## Execution

After this plan is approved:

1. Create worktree per `superpowers:using-git-worktrees`.
2. Per-PR: run `superpowers:subagent-driven-development` for TDD execution.
3. Per-PR: run quorum self-review on changed files (Phase 6 of dev:start).
4. Per-PR: record calibrator verdicts via `mcp__quorum__feedback` (Phase 7).
5. After each PR merges: run ablation harness; record findings.
6. After all PRs: bump version (likely v0.19.0 — significant schema change), update CHANGELOG, draft release notes.
