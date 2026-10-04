# Repo-defined review config (#632)

A repository states what it wants reviewed, where, and what it already knows about itself. One file, `.quorum/review.toml`, read once per run from the project root (where `.quorum/suppress.toml` is read today, `main.rs:2939`).

## What it expresses, first slice

```toml
# .quorum/review.toml

# Facts about the project that a reviewer cannot read from one file. Rendered
# into <review_context> for every axis as "Project notes", so they reach the
# model before the code rather than suppressing findings after.
[project]
notes = [
  "This is a binary crate; the lib target has no out-of-tree consumers, so adding a required field to a pub struct breaks nothing downstream.",
  "eval/axes/corpus/ is deliberately defective fixture code; findings there are expected.",
]

# Which axes run where. Resolved per file: --axes on the command line wins,
# then the most specific matching scope (longest glob), then the mode default.
# `axes = []` means the file is not reviewed by the LLM at all (AST rules still run).
[[scope]]
paths = ["src/**"]
axes = "default"

[[scope]]
paths = ["tests/**", "src/**/tests.rs"]
axes = ["testing-antipatterns", "correctness"]

[[scope]]
paths = ["eval/corpus/**", "eval/axes/corpus/**", "rules/**/tests/**"]
axes = []

[[scope]]
paths = ["src/llm_client.rs", "src/redact.rs", "src/skill_prompt_defense.rs"]
axes = "audit"

# Unchanged semantics from .quorum/suppress.toml; both files are read, this
# one first. suppress.toml keeps working so nothing breaks on upgrade.
[[suppress]]
pattern = "struct literal"
reason = "the lib crate has no out-of-tree consumers; every struct literal is updated in the same change"
```

Not in the first slice: per-scope extra instructions for an axis (the "repo-defined rules" phrasing on the issue), unstructured docs as a source of notes, and inheritance across nested `.quorum/` directories. Each is a follow-up once the shape above has run on this repo for a week.

## Where it plugs in

| piece | change |
|---|---|
| `src/review_config.rs` (new, lib) | `ReviewConfig { project: Project, scopes: Vec<Scope>, suppress: Vec<SuppressionRule> }`, `load(project_root) -> ReviewConfig` (missing file = empty; parse error = warning + empty, like suppressions), `axes_for(path) -> Option<AxisSelection>` picking the longest matching glob, `notes_block() -> Option<String>`. Reuses `suppress::SuppressionRule` and the `glob::MatchOptions` convention from `suppress.rs:65`. Reserved names `default`/`audit` resolve through the same table as `--axes` (`CODE_MODE_MACRO_AXES`, `AUDIT_AXES` move to the lib so both callers share them). |
| `main.rs` axis resolution | `resolve_axes` stays per run and yields the *baseline*. A new per-file step narrows it: when no `--axes` was given and a scope matches, the scope's set replaces the baseline for that file; `axes = []` skips the matrix for the file. The audit row records `AxisSelectionSource::RepoScope`. |
| `skill_executor::expand_matrix` | unchanged; it already builds cells per (file, skill). The per-file skill list is passed in rather than the run-wide one. |
| `<review_context>` | `pipeline::render_context_for_axes` gains a "Project notes" section from `notes_block()`, placed first. Also rendered on the legacy path so `--deep` sees it. |
| suppressions | `load_project_suppressions` reads `review.toml`'s `[[suppress]]` then `suppress.toml`, concatenated. |
| `AxisSelectionSource` | gains `RepoScope`; `stats --skills` prints a per-source count so the field has a reader (#644's review noted it has none). |

## Tests (RED first, each mutated once)

1. `review_config::tests`: parse the sample above; `axes_for("src/redact.rs")` is `audit` (longest glob wins over `src/**`); `axes_for("eval/corpus/x.py")` is `Some(empty)`; `axes_for("README.md")` is `None`; unknown axis name in a scope is a load-time error naming the scope; `default`/`audit` expand to the same lists the CLI uses.
2. `tests/cli.rs`, cassette-backed, through `tests/support`: a project with `review.toml` scoping `tests/**` to `testing-antipatterns` reviews `tests/t.rs` with one call and `src/lib.rs` with three; `--axes correctness` overrides to one call each; `axes = []` on a path yields zero calls and the file still gets AST findings.
3. Wire test: `[project] notes` appear in the system message inside `<review_context>` under "Project notes", for every axis, and are absent when the section is empty.
4. Suppressions from `review.toml` and `suppress.toml` both apply (extend `suppressed_hidden_finding_is_not_recorded`'s fixture).
5. `stats --skills` shows `repo_scope` in the selection-source breakdown after such a run (consumer for the new variant).
6. Guard: the reserved-name table has one definition (a source scan that `CODE_MODE_MACRO_AXES`/`AUDIT_AXES` appear once).

## Measurement

Run `eval/axes/run_axes.py` unchanged before and after: scopes must not change in-lane recall (the corpus has no `review.toml`). Then add a `review.toml` to this repository: `eval/**` and `rules/**/tests/**` to `axes = []`, the trust-boundary files to `audit`, the struct-literal note under `[project]`, and the two suppress rules moved over. Review the next three PRs with it and record what the note changes versus the suppression (does the "struct literal" finding stop appearing at all, which is the point).

## Risks

- A scope that silently excludes a path hides review. Mitigation: the summary line says `N file(s) not reviewed by repo scope`, the JSON `_meta` lists them, and the exit code is unaffected.
- Longest-glob precedence can surprise (`src/**` vs `src/**/tests.rs`). Mitigation: `quorum review --explain-scope <file>` prints which scope matched and why; cheap, and the test for it is the consumer the flag needs.
- Project notes are prompt text under the repo's control, which is also the attacker's control on a fork PR. They go through `defang_sandbox_tags` like every other injected block and are capped at 2 KiB.
