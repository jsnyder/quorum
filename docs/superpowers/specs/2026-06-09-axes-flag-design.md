# Design: --axes flag + code mode macro (#417)

**Issue:** #417 (part of #403 multi-axis review skills framework)
**Date:** 2026-06-09
**Status:** Draft

## Summary

Wire the skill executor, integrator, and bundled skill manifests into `run_review` behind a new `--axes` CLI flag. This is the first user-visible milestone: `quorum review file.rs` runs three specialist reviews (correctness, security, testing-antipatterns) in parallel, dedupes/merges their findings, and outputs the integrated result.

## CLI Surface

### New flag

```
--axes <a,b,c>    Comma-separated skill names (e.g. correctness,security)
```

### Axis resolution (evaluated in order)

| Condition | Resolved axes | `axis_selection_source` |
|-----------|--------------|------------------------|
| `--axes a,b` explicitly provided | `[a, b]` | `ExplicitAxes` |
| `--mode code` (default), no `--axes`, no legacy flags | `[correctness, security, testing-antipatterns]` | `ModeMacro` |
| `--deep`, `--daemon`, or `--ensemble` active (no explicit `--axes`) | No axes — fall back to legacy single-prompt LLM path | `Legacy` |
| `--mode plan\|docs\|tests\|release` | Hard error | N/A |

### AxisSelectionSource enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisSelectionSource {
    ExplicitAxes,
    ModeMacro,
    Legacy,
}
```

### Reserved mode error

```
error: mode 'plan' requires axes not installed in this version: [plan-coherence, plan-completeness]
```

Placeholder skill names per reserved mode:
- `plan` → `plan-coherence, plan-completeness`
- `docs` → `docs-accuracy, docs-completeness`
- `tests` → `test-coverage, test-quality`
- `release` → `release-readiness`

### Validation

- Each axis name must match a loadable skill manifest (bundled `skills/` or user `~/.quorum/skills/`).
- Unknown axis → hard error listing available skills.
- `--axes` with a reserved `--mode` → hard error (reserved modes don't have skills yet).
- `--axes` explicitly combined with `--deep`, `--daemon`, or `--ensemble` → hard error: "multi-axis review is not supported with --deep/--daemon/--ensemble yet".
- `--axes` explicitly provided with no API key → warning: "warning: --axes requires an LLM client; running AST-only review".

### Legacy flag fallback

When `--deep`, `--daemon`, or `--ensemble` is active and no explicit `--axes` is given, default axis resolution is **suppressed**. The review falls back to the existing single-prompt LLM path. This avoids breaking existing users of these flags. Explicit `--axes` with these flags is a hard error (unsupported combination).

## Architecture

### Integration approach: thin shim in `run_review`

The existing `run_review` function in `main.rs` gains a new code path after AST analysis. The skill executor replaces the single-prompt LLM review path when axes are resolved.

```
run_review(opts)
  ├── [unchanged] trace init, config load, LLM client build
  ├── [new] resolve_axes(opts.axes, opts.mode, opts.deep/daemon/ensemble)
  │         → Option<(Vec<LoadedSkill>, AxisSelectionSource)>
  │         Returns None when legacy flags suppress default resolution
  ├── [new] if axes resolved: build SkillExecutorConfig
  ├── for each file:
  │     ├── [unchanged] AST analysis → local findings
  │     ├── [unchanged] linter findings
  │     ├── [new] context enrichment (project context + Context7) → enriched_source
  │     ├── if axes resolved:
  │     │     ├── [new] execute_matrix(skills × [model] × file, enriched_source)
  │     │     ├── [new] integrate(cell_results) → IntegratorOutput
  │     │     └── [new] calibrator on integrated findings
  │     ├── else (legacy path):
  │     │     ├── [unchanged] single-prompt LLM review
  │     │     └── [unchanged] calibrator on LLM findings
  │     └── [unchanged] merge AST + LLM/skill findings → FileReviewResult
  ├── [unchanged] output formatting (JSON/compact/human)
  └── [unchanged] telemetry recording
```

### Context injection threading

Context enrichment (project context via `ContextInjector` + Context7 framework docs) runs **before** the skill executor call in the per-file loop. The enriched source text is what gets passed to `wrap_code_to_review()` and then to each executor cell. This matches the current flow where context injection happens inside `review_file()` before the LLM call — the only change is that the enriched source now feeds into multiple skill cells instead of one prompt.

### Calibrator positioning

In the current single-prompt path, the calibrator runs inside `review_file()` on per-file findings. In the new multi-axis path, the calibrator runs **after** `integrate()` on the merged/deduped findings. This is a behavior change: the calibrator now sees the integrator's confidence scores (noisy-or fused) rather than raw LLM confidence. This is intentional — the integrator's merged confidence is a better signal for calibration than per-cell raw confidence.

### LlmReviewer adapter

`skill_executor.rs` defines a lib-local `LlmReviewer` trait. The binary-side `OpenAiClient` implements the binary-side `pipeline::LlmReviewer` trait. A thin adapter bridges the two:

```rust
struct SkillLlmAdapter(Arc<OpenAiClient>);

impl skill_executor::LlmReviewer for SkillLlmAdapter {
    fn review(&self, prompt: &str, model: &str, system_prompt: &str)
        -> anyhow::Result<skill_executor::LlmResponse>
    {
        let resp = pipeline::LlmReviewer::review(&*self.0, prompt, model, system_prompt)?;
        Ok(skill_executor::LlmResponse {
            content: resp.content,
            usage: skill_executor::TokenUsage {
                prompt_tokens: resp.usage.prompt_tokens,
                completion_tokens: resp.usage.completion_tokens,
                cache_read_tokens: resp.usage.cache_read_tokens,
            },
        })
    }
}
```

### Skill loading

`skill_manifest::load_skills()` searches:
1. Bundled skills: `<binary_dir>/skills/*.toml` and compile-time `include_str!` fallback
2. User skills: `~/.quorum/skills/*.toml`

User skills with the same `name` as a bundled skill override the bundled version (user takes precedence).

Resolved axes are filtered against loaded skills. Missing skills produce a hard error listing available skills.

### Matrix construction

For each file, the skill executor builds the matrix:
- **Skills dimension:** resolved axes (1-N skills)
- **Models dimension:** `[cfg.model]` (single model; ensemble support is out of scope)
- **Files dimension:** single file (executor runs per-file in the outer loop)

Each cell gets:
- System prompt: `wrap_skill_instructions(skill.prompts, model_family)`
- User prompt: `wrap_code_to_review(enriched_source, file_path)`
- Model: from config
- Skill metadata: name, version, manifest SHA256

### Parallelism model

Two levels of parallelism operate independently:

1. **File-level:** The existing `run_review` loop processes files using the `--parallel` semaphore. Multiple files can be in-flight concurrently.
2. **Skill-level (within a file):** The skill executor runs cells (skills × models) concurrently using its internal `BudgetTracker`. All skill cells for a single file execute in parallel.

The `--parallel` flag controls file-level concurrency. Skill-level concurrency within a file is bounded by the executor's budget (token cap) and the number of skills. With 3 bundled skills and `--parallel 4`, peak concurrent LLM calls is 12 (4 files × 3 skills).

### Integration

Cell results are converted to `TaggedFinding` and passed to `integrate()`:
- Cluster by `(file_path, finding_kind)`
- Noisy-or confidence fusion within clusters
- Suppress below confidence floor (0.30)
- Clamp severity to skill's `max_severity`
- Deterministic sort: severity desc, confidence desc, line_start asc, title asc

## Traceability

### Structured tracing (`--trace`)

The existing `--trace` flag writes structured spans to `~/.quorum/trace.jsonl`. New `tracing::info!` spans added for:
- Axis resolution: which axes resolved, source (`ExplicitAxes`/`ModeMacro`/`Legacy`)
- Skill executor invocation: skill count, model, file path
- Integrator output: findings count, suppressed count, clusters formed

The skill executor and integrator already emit `tracing::warn`/`tracing::debug` calls that flow through the existing trace subscriber.

### Audit logging

- `~/.quorum/skill_invocations.jsonl`: one row per executor cell (skill × model × file), with `axis_selection_source`
- `~/.quorum/integrator_decisions.jsonl`: one row per integrator cluster
- `~/.quorum/skills.lock`: manifest checksums for reproducibility
- `skill_run_id` links findings → audit records → integrator decisions

### Out of scope: `--trace-prompts` (#412)

Full prompt forensic capture (actual LLM prompts/responses) is deferred to #412.

## Output & Telemetry

### Finding identity

Integrated findings carry: `originating_skill`, `skill_version`, `manifest_sha256`, `skill_run_id`, `clamped_from_severity`. These appear in JSON output and are available for `--by-skill` stats (#421).

### Telemetry

Token usage from all cells summed into existing `tokens_in/out` on `TelemetryEntry`. Per-cell breakdown lives in the audit log only.

### Exit codes

Unchanged: 0=clean, 1=warnings, 2=critical, 3=tool error.

## Files Changed

| File | Change |
|------|--------|
| `src/cli/mod.rs` | Add `--axes` flag to `ReviewOpts` |
| `src/main.rs` | Axis resolution, legacy fallback, skill loading, executor/integrator wiring in `run_review`, `SkillLlmAdapter` |
| `src/skill_audit.rs` | Add `axis_selection_source: AxisSelectionSource` field to `SkillInvocationRecord` |

## Out of Scope

- #418: Legacy single-prompt fallback flag (`--no-skills` or similar)
- #412: `--trace-prompts` forensic capture
- #413: Skill fixture / smoke-test harness
- Non-code mode macros (plan, docs, tests, release skill bundles)
- Per-skill calibrator weighting

## Testing Strategy

1. **Unit tests for axis resolution:** explicit axes, mode macro default, reserved mode errors, unknown axis errors, legacy flag fallback (--deep/--daemon/--ensemble suppress default), explicit --axes + legacy flag error
2. **Unit tests for LlmReviewer adapter:** verify type mapping between binary-side and lib-side types
3. **Unit test for skill loading precedence:** user skill overrides bundled skill with same name
4. **Integration test:** mock LlmReviewer, run full pipeline (AST + skills + integrator), verify output contains findings from multiple skills with correct metadata
5. **CLI integration test:** `quorum review --axes security <file>` with no API key → AST-only with warning. With mock → skill findings in output.
6. **Traceability test:** verify `axis_selection_source` is recorded correctly in audit records for each resolution path
