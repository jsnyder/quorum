# --axes Flag Test Strategy (Reconciled)

> Reconciled from test-planning agent + antipattern review agent outputs.
> Antipattern fixes are marked with [AP-fix].

## Acceptance Criteria

| AC | Pass/Fail Condition |
|----|---------------------|
| AC-1 | `resolve_axes()` returns correct `Option<ResolvedAxes>` for every combination of `--axes`, `--mode`, `--deep`/`--daemon`/`--ensemble` |
| AC-2 | Legacy flags without explicit `--axes` take the existing single-prompt LLM path with zero behavioral change |
| AC-3 | When axes resolve, per-file loop calls `execute_matrix()` then `integrate()` instead of single-prompt LLM. Findings carry `originating_skill`, `skill_version`, `manifest_sha256`, `skill_run_id` |
| AC-4 | Integrator clusters by `(file_path, finding_kind)`, fuses confidence via noisy-or, suppresses below 0.30 floor, clamps severity to `max_severity` |
| AC-5 | AST/linter findings produced regardless of axis resolution |
| AC-6 | Reserved modes (`tests`, `release`) produce hard error naming placeholder skills. Unknown axes list available skills. |
| AC-7 | `--axes` with no API key emits warning and produces AST-only output |
| AC-8 | Token usage from all executor cells summed into `tokens_in`/`tokens_out` telemetry |
| AC-9 | `axis_selection_source` recorded in `skill_invocations.jsonl` |

## Test Cases by Component

### A: `resolve_axes()` (Task 4)

| ID | Input | Expected |
|----|-------|----------|
| A1 | `--axes security`, mode=Code, no legacy | `Some(skills=[security], source=ExplicitAxes)` |
| A2 | `--axes correctness,security`, mode=Code | `Some(skills=[correctness,security], source=ExplicitAxes)` |
| A3 | no axes, mode=Code, no legacy | `Some(skills=[correctness,security,testing-antipatterns], source=ModeMacro)` |
| A4 | no axes, mode=Code, `--deep`=true | `None` (legacy fallback) |
| A5 | no axes, mode=Code, `--daemon`=true | `None` (legacy fallback) [AP-fix: was missing] |
| A6 | no axes, mode=Code, `--ensemble`=true | `None` (legacy fallback) [AP-fix: was missing] |
| A7 | `--axes security`, `--deep`=true | `Err` containing "not supported with --deep" |
| A8 | `--axes security`, `--ensemble`=true | `Err` containing "not supported with --ensemble" [AP-fix: was missing] |
| A9 | `--axes security`, `--daemon`=true | `Err` containing "not supported with --daemon" [AP-fix: was missing] |
| A10 | no axes, mode=Tests | `Err` containing "requires axes not installed" and "test-coverage" |
| A11 | no axes, mode=Release | `Err` containing "requires axes not installed" and "release-readiness" |
| A12 | `--axes nonexistent`, mode=Code | `Err` containing "unknown skill axis" and listing available skills |
| A13 | no axes, mode=Plan, no legacy | `None` (prose mode uses legacy) |
| A14 | no axes, mode=Docs, no legacy | `None` (prose mode uses legacy) |
| A15 | `--axes SECURITY` (uppercase) | `Some(skills=[security])` -- case-insensitive [AP-fix: was missing] |
| A16 | `--axes ""` or empty strings | `Err` or filtered out [AP-fix: edge case from test-planning] |
| A17 | `--axes security,security` | Deduplicated to 1 skill + warning [AP-fix: edge case from test-planning] |
| A18 | empty available_skills, mode=Code | `Err` about missing bundled skills [AP-fix: was missing] |
| A19 | `--deep`=true, `--daemon`=true (multi-flag) | `None` -- first flag reported if combined with --axes [AP-fix: priority bug] |

**Deleted:** `explicit_axes_with_no_api_key_still_resolves` (test 8 in plan). It duplicates A1 since resolve_axes doesn't check API keys. [AP-fix: non-test removed]

### B: `AxisSelectionSource` enum (Task 1)

| ID | Input | Expected |
|----|-------|----------|
| B1 | Serialize `Legacy` to JSON | `"legacy"` |
| B2 | Deserialize `"legacy"` from JSON | `AxisSelectionSource::Legacy` |
| B3 | Roundtrip all 5 variants | Each survives serialize-then-deserialize |

Note: serde roundtrip is acceptable as a format contract guard (not testing serde itself, testing our rename_all annotation). [AP-fix: documented rationale]

### C: `ReviewMode` reserved variants (Task 2)

| ID | Input | Expected |
|----|-------|----------|
| C1 | `"tests".parse::<ReviewMode>()` | `Ok(ReviewMode::Tests)` |
| C2 | `"release".parse::<ReviewMode>()` | `Ok(ReviewMode::Release)` |
| C3 | `ReviewMode::Tests.is_reserved()` | `true` |
| C4 | `ReviewMode::Release.is_reserved()` | `true` |
| C5 | `ReviewMode::Code.is_reserved()` | `false` |
| C6 | `ReviewMode::Plan.is_reserved()` | `false` |
| C7 | `ReviewMode::Docs.is_reserved()` | `false` |

### D: `SkillLlmAdapter` (Task 5)

| ID | Input | Expected |
|----|-------|----------|
| D1 | Mock OpenAiClient returning valid response | Adapter maps `content`, `prompt_tokens`, `completion_tokens`, `cached_tokens` correctly |
| D2 | Mock returning `usage: None` | Token counts default to 0, no panic |
| D3 | Mock returning error | Error propagated unchanged |

### E: Integration (Task 9)

| ID | Input | Expected |
|----|-------|----------|
| E1 | Mock LlmReviewer + 2 skills + source | `execute_matrix` returns 2 `CellResult`s with `originating_skill` set |
| E2 | Same as E1 through integrator | Duplicate findings merged; `assert_eq!(output.findings.len(), expected_exact_count)` [AP-fix: exact count not `< 4`] |
| E3 | Assert on finding content | Each finding has title, severity, line_start populated [AP-fix: content not just `.is_empty()`] |
| E4 | Finding with confidence 0.20 | Suppressed (below 0.30 floor) |
| E5 | Severity=Critical on skill with max_severity=Medium | Clamped to Medium, `clamped_from_severity` set |
| E6 | Token usage from 3 cells: (100,50,0), (200,80,10), (150,60,5) | Summed: prompt=450, completion=190, cache_read=15 |

### F: Skill loading precedence (existing tests, verify)

| ID | Input | Expected |
|----|-------|----------|
| F1 | Bundled + user `correctness.toml` | User version wins |
| F2 | Bundled `security.toml`, no user override | Bundled loaded |
| F3 | User `custom-lint.toml` not in bundled | Available alongside bundled |

### G: CLI integration (Task 10 smoke tests)

| ID | Input | Expected |
|----|-------|----------|
| G1 | `quorum review file.rs --axes security` (no API key) | Exit 0/1, stderr warning |
| G2 | `quorum review file.rs --mode tests` | Exit 3, stderr "requires axes not installed" |
| G3 | `quorum review file.rs --axes security --deep` | Exit 3, stderr "not supported with --deep" |

## Mock Simplification [AP-fix]

The `mock_skill()` helper should use `..Default::default()` to reduce 15-field boilerplate. Only set `name` (and `max_severity` when testing severity clamping). Requires `#[derive(Default)]` on `SkillManifest` — if not already present, the implementing agent should add it or use a builder.

## Risk Areas

1. **`resolve_axes` priority ordering** — 6 branches evaluated in sequence; wrong order silently misroutes
2. **Token usage field mapping** — binary-side `cached_tokens` vs lib-side `cache_read_tokens` name mismatch
3. **Calibrator input change** — now sees noisy-or fused confidence instead of raw LLM confidence
4. **`llm_for_pipeline = None`** — must not break LLM-only review for unsupported file types
5. **`Arc` trait bounds** — `LoadedSkill` must be `Clone + Send + Sync` for parallel path
6. **Parallel path duplication** — sequential and parallel loops have identical branching logic (regression risk)
7. **Integrator determinism** — same input must produce byte-identical output across runs
