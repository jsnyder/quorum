# Simplicity Skill Axis Design

**Inspired by:** [ponytail](https://github.com/DietrichGebert/ponytail) (MIT license)

## Problem

Quorum's current default axes (correctness, security, testing-antipatterns) catch bugs and vulnerabilities but not overengineering. Code that works correctly but is unnecessarily complex — hand-rolled stdlib equivalents, abstractions with one implementation, 20-line functions that could be 2 — passes review unchallenged.

## Solution

Add a `simplicity` skill axis that catches unnecessary complexity. The prompt encodes ponytail's 6-rung ladder: does this need to exist → stdlib does it → platform does it → dependency does it → one line → minimum that works.

### Manifest: `skills/simplicity.toml`

- `name = "simplicity"`
- `axis = "simplicity"`
- `max_severity = "medium"` — complexity is never critical/high
- `target_findings = 8` — fewer than correctness (10) since simplicity findings are lower priority
- Credit in description: "Inspired by ponytail (MIT, github.com/DietrichGebert/ponytail)"
- Prompt tags findings as: `yagni`, `stdlib`, `native`, `shrink`, `delete`

### Default axes

Add `"simplicity"` to `CODE_MODE_MACRO_AXES` in `src/main.rs:977`:
```rust
const CODE_MODE_MACRO_AXES: &[&str] = &["correctness", "security", "testing-antipatterns", "simplicity"];
```

### Embedding

Add to `EMBEDDED_SKILLS` in `src/skill_manifest.rs`.

### dev:start integration

Add ponytail-review as a step in Phase 6 of the dev:start user skill (after quorum review, before finishing). This runs the ponytail plugin's complexity-reduction review on changed files.

## Files changed

| File | Change |
|------|--------|
| `skills/simplicity.toml` | New skill manifest with prompt |
| `src/main.rs:977` | Add "simplicity" to CODE_MODE_MACRO_AXES |
| `src/skill_manifest.rs` | Add to EMBEDDED_SKILLS |
| `~/.claude/skills/dev:start` (user settings) | Add ponytail-review step |

## What this does NOT do

- Does not copy ponytail's prompt verbatim — writes an original prompt tuned for quorum's finding format
- Does not flag correctness bugs, security issues, or missing tests (those have their own axes)
- Does not flag validation, error handling, or accessibility (ponytail's "never on the chopping block" rule)
