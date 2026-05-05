# Prose Review Modes for Quorum

**Date:** 2026-05-05
**Status:** Reviewed by GPT-5.4 (via PAL), revised

## Problem

PAL MCP server has been the go-to for reviewing non-code artifacts (plans, docs, specs) via different LLMs, but it keeps breaking (routing drift, expired keys, LiteLLM incompatibilities). Actual usage pattern is identical to quorum: file-in, structured-findings-out, single pass. No meaningful multi-turn conversations observed across months of episodic memory.

Quorum already has the LLM client, ensemble support, feedback/calibration infrastructure, and structured output. The gap is: prompts tuned for non-code content, and skipping the code-specific pipeline stages.

## Design

### Review Modes

A new `--mode` flag selects the review lens. Default remains `code` (current behavior). Two new modes in v1:

```
quorum review docs/plans/my-plan.md --mode plan
quorum review docs/ARCHITECTURE.md --mode docs
```

**Mode enum (v1):**

| Mode | Focus | Categories Emphasized |
|------|-------|----------------------|
| `code` | Current behavior (default) | All 10 |
| `plan` | Feasibility, gaps, risks, missing acceptance criteria, scope creep, contradictions | Correctness, Logic, Validation, Robustness |
| `docs` | Clarity, completeness, accuracy, internal consistency, stale references | Correctness, Validation, Maintainability |

`spec` deferred to v2 — its rubric overlaps significantly with `plan`. Add it when the review rubrics demonstrably diverge.

`--mode` stays as a flag, not a positional subcommand. One mental model: `quorum review <files> [options]`.

Explicit `--mode` required for non-code files in v1 — no auto-detection from `.md` extension. Avoids accidental prose review with the wrong rubric.

### Input Constraints

`--mode` applies per-invocation, not per-file. Mixed artifact types (e.g., `quorum review src/*.rs docs/plan.md --mode plan`) are rejected with a clear error in v1. Directory/glob expansion must produce homogeneous inputs for the selected mode.

### Pipeline Changes

When `mode != Code`:

1. **Skip**: local AST analysis, ast-grep scanning, linter invocation, hydration, grounding
2. **Keep**: LLM review (with mode-specific prompt), merge (for ensemble dedup), calibration, output formatting
3. **Skip by default**: Context7 enrichment — prose modes do not inject framework docs unless `--context7` is explicitly passed. Plans referencing "React" don't need React API docs; it biases toward framework trivia over reviewing the document on its own terms.

The skip logic follows the existing `context7_skip_reason` predicate pattern in `pipeline.rs`. A `pipeline_stages_for_mode()` function returns which stages run.

### Finding Schema

No schema changes needed. Prose findings still have line numbers (markdown has lines), and the existing Category enum covers the relevant domains. `line_start`/`line_end` reference the input file's line numbers.

`Source` variants remain the same — findings come from `Llm(model_name)` since AST/linter stages are skipped.

Prose prompts explicitly permit broad line ranges and multiple evidence citations within the `evidence` array, since prose defects (contradictions, omissions, terminology drift) often span distant sections rather than contiguous code blocks.

### Prompt Templates

Each mode gets a system prompt template stored as a const in a new `src/prose_prompts.rs` module. Templates follow quorum's existing prompt structure:

- Role framing (e.g., "You are reviewing an implementation plan...")
- Mode-specific rubric (what to look for)
- Output format (same JSON finding schema as code review)
- Anti-anchoring instruction (same as code review)
- Category constraint (only emit categories relevant to the mode)
- Evidence guidance: use line numbers from the source document, cite relevant text in `evidence`, allow wide line ranges and cross-section references for contradictions/omissions

### Ensemble / Consensus

`--ensemble` works identically — fan out to N models, merge findings by semantic similarity. This directly replaces PAL's consensus workflow.

### Feedback and Calibration

Feedback recording works identically. `quorum feedback --file plan.md --finding "..." --verdict fp` records to the same feedback store.

**Calibration is mode-aware.** The calibrator includes `mode` in precedent lookup: same-mode precedents are preferred over cross-mode. A code FP precedent should not suppress the same-text finding in a plan review — the review context is materially different. If same-mode precedents are sparse, cross-mode precedents serve as fallback with reduced weight.

`ReviewRecord` in `reviews.jsonl` gains an optional `mode` field (defaults to `"code"` for backward compat via `serde(default)`). `mode` is tracked in telemetry from Phase 1, not deferred to Phase 3.

### CLI Surface

```
quorum review <files> --mode <code|plan|docs>   # mode-specific review
quorum review <files> --mode plan --ensemble     # multi-model plan review
quorum review <files> --mode plan --model gemini-2.5-pro  # specific model
quorum review <files> --mode docs --context7     # opt-in Context7 for prose
```

No new subcommands. `--mode` is the only addition to the CLI surface.

### What This Replaces

| PAL workflow | Quorum equivalent |
|-------------|-------------------|
| `pal codereview` | `quorum review --mode code` (existing) |
| `pal analyze` / plan review | `quorum review --mode plan` |
| `pal consensus` | `quorum review --ensemble` |
| `pal docgen` | Out of scope (generation, not review) |
| `pal thinkdeep` / `pal challenge` | Out of scope for v1 (multi-turn) |

### What This Does NOT Do

- No multi-turn conversations (file-in, findings-out only)
- No document generation (review only)
- No template/rubric customization in v1 (hardcoded prompts per mode)
- No auto-detection of mode from content (explicit `--mode` required)
- No mixed artifact types per invocation
- No `spec` mode in v1 (deferred until rubric diverges from `plan`)

### Future Extensions (not in v1)

- `spec` mode when plan/spec rubrics demonstrably diverge
- User-defined review templates in `~/.quorum/templates/`
- `--mode custom --template my-rubric.md` for ad-hoc review criteria
- Multi-turn follow-up mode (the thing PAL had but nobody used)
- Per-mode category display labels if analytics demand it

## Implementation Phasing

**Phase 1: Scaffold** — `ReviewMode` enum, `--mode` CLI flag, pipeline stage gating, `mode` field in `ReviewRecord`/telemetry. No LLM prompt changes yet (uses code prompt for all modes as a baseline).

**Phase 2: Prompts + Quality Gate** — Mode-specific prompt templates in `src/prose_prompts.rs`. Mode-aware calibration wired into precedent lookup. Validate against a benchmark corpus before enabling in docs/help:

- Benchmark: 5-10 plans + 5-10 docs from this repo, each with manually adjudicated expected findings
- Metrics: precision-at-top-3 (per mode), false-positive ceiling (<40%), schema compliance rate (>95%), empty-array correctness on clean docs
- Test across at least 2 target models (e.g., gpt-5.4 + gemini-2.5-pro) for regression
- Manual adjudication procedure: author reviews findings, marks tp/fp/partial, records to feedback store
- Release gate: mode is available via `--mode` but not listed in default `--help` until benchmarks pass

**Phase 3: Polish** — Stats breakdown by mode, feedback loop validation across modes, evaluate Context7 opt-in quality with A/B comparison.

## Prompt Design Rationale

Draft prompts in `src/prose_prompts.rs`. Design informed by:

- **Anthropic eval patterns** (Context7: platform.claude.com/docs/test-and-evaluate): Rubric in XML tags, structured JSON schema enforcement, thinking-before-result patterns
- **OpenAI Cookbook LLM-as-judge** (Context7: openai-cookbook): Criteria → Steps → Evaluation Form template; explicit scoring rubric with graduated severity bands; completeness + quality modifier pattern
- **Quorum's own code review prompt** (`src/llm_client.rs:1049-1141`): The proven structure we're adapting

Key prompt engineering decisions:

1. **Same XML-tagged structure as code review**: `<review_spec>`, `<severity_rubric>`, `<categories>`, `<response_format>`, `<suggested_fix_policy>`, `<output_hygiene>`. Maintains prompt caching compatibility and consistent LLM behavior.

2. **Evaluation criteria as ordered priority list** (from OpenAI Cookbook pattern): Each mode's `<review_spec>` lists dimensions in priority order, mirroring how the code review prompt orders "critical defects → security → logic → architecture." This gives the LLM a clear triage framework rather than a flat checklist.

3. **Mode-specific severity calibration**: Plan severity maps to "impact on successful implementation" (not production crashes). Docs severity maps to "impact on the reader" (not system safety). Down-classification rules adapted per mode — e.g., "if the plan is explicitly a draft, focus on structural issues."

4. **Concrete suggested_fix policy per mode**: Plans get "write the specific criteria / state the correct dependency order." Docs get "provide the corrected statement / write the missing step." Same anti-advisory stance as code review ("do not write 'consider' or 'review this'").

5. **Reduced category set per mode**: Plan uses 5 categories (correctness, logic, validation, robustness, maintainability) with mode-specific definitions. Docs uses 3 (correctness, validation, maintainability). Fewer categories reduce misclassification and improve calibrator precision.

6. **Evidence array for cross-section issues**: Both prompts explicitly instruct the LLM to use `line_start`/`line_end` for the primary location and `evidence` array for quoting related text from other sections — addressing the "prose findings span distant sections" problem.

7. **Same prompt injection hardening**: `<untrusted_data_warning>` adapted for `<document>` blocks instead of `<untrusted_code>`. Same sandbox-tag defanging applies via `defang_sandbox_tags()`.

## Decisions (from frontier review)

1. `--mode` stays as a flag (not positional/subcommand)
2. Explicit `--mode` required for non-code in v1 (no auto-detect)
3. Ship 3 modes: `code`, `plan`, `docs` — add `spec` in v2 if needed
4. Calibration is mode-aware from day one
5. Context7 off by default for prose modes
6. Mixed inputs rejected per-invocation in v1
7. Telemetry captures `mode` from Phase 1
8. Reviewing `.md`/`.txt`/`.adoc`/`.rst` without `--mode` errors with hint: "This looks like a prose file. Use --mode plan or --mode docs to review it."
9. Calibrator weighting: same-mode precedents weighted 1.0, cross-mode fallback weighted 0.5 (initial heuristic, tunable after data)
