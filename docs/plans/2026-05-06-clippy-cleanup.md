# Clippy Cleanup Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all 166 clippy errors so `cargo clippy --workspace --all-targets --locked -- -D warnings` passes on stable, then remove `continue-on-error: true` from the CI clippy job.

**Architecture:** Two-pass approach — `cargo clippy --fix` for the ~155 mechanical lints, then manual triage of the ~11 remaining. No behavioral changes; all fixes are semantics-preserving refactors.

**Tech Stack:** Rust 1.95 stable, cargo clippy, GitHub Actions CI

**Closes:** #214, #182 (duplicate)

---

### Task 1: Auto-fix mechanical lints

**Files:**
- Modify: `src/*.rs` (many files, auto-applied)

**Step 1: Run cargo clippy --fix**

```bash
cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged -- -D warnings
```

This auto-fixes ~155 lints across these categories:
- `collapsible_if` (129) — nested ifs → let-chains
- `field_reassign_with_default` (10) — use struct-update syntax
- `useless_vec` (7) — `&vec![...]` → `&[...]`
- `collapsible_str_replace` (1)
- `get_first` (1) — `.get(0)` → `.first()`
- `double_ended_iterator_last` (1)
- `manual_strip` (1)
- `manual_repeat_n` (1)
- `manual_contains` (1)
- `unnecessary_map_or` (1)
- `needless_question_mark` (1)
- `unnecessary_to_owned` (1)
- `empty_line_after_doc_comments` (1)

**Step 2: Run clippy again to see what remains**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | grep '^error' | head -30
```

Expected: only the manual-triage lints remain (~11).

**Step 3: Run tests to verify no regressions**

```bash
cargo test --bin quorum
```

Expected: all tests pass (these are semantics-preserving transforms).

**Step 4: Commit**

```bash
git add -A
git commit -m "chore: apply cargo clippy --fix for 155 mechanical lint fixes

Categories: collapsible_if (129), field_reassign_with_default (10),
useless_vec (7), and 9 single-instance lints.

Closes #182"
```

---

### Task 2: Fix `derivable_impls` — Provenance default

**Files:**
- Modify: `src/feedback.rs:29-56` (enum + manual Default impl)

**Step 1: Verify existing tests pass**

```bash
cargo test --bin quorum -- provenance
```

**Step 2: Replace manual Default impl with derive**

In `src/feedback.rs`, add `#[derive(Default)]` to the `Provenance` enum and add `#[default]` to the `Unknown` variant. Remove the manual `impl Default for Provenance` block.

**Step 3: Run tests**

```bash
cargo test --bin quorum -- provenance
```

Expected: PASS

**Step 4: Commit**

```bash
git add src/feedback.rs
git commit -m "chore: derive Default for Provenance instead of manual impl"
```

---

### Task 3: Fix `new_without_default` — FindingBuilder

**Files:**
- Modify: `src/finding.rs:~140` (add Default impl)

**Step 1: Add Default impl that delegates to new()**

```rust
impl Default for FindingBuilder {
    fn default() -> Self {
        Self::new()
    }
}
```

Note: `FindingBuilder::new()` generates a ULID, so each `default()` call produces a unique builder. This is intentional — the builder is a transient construction aid, not a reusable value.

**Step 2: Run tests**

```bash
cargo test --bin quorum -- finding
```

Expected: PASS

**Step 3: Commit**

```bash
git add src/finding.rs
git commit -m "chore: add Default impl for FindingBuilder (delegates to new)"
```

---

### Task 4: Fix `duplicate_macro_attributes` — domain.rs

**Files:**
- Modify: `src/domain.rs:~403` (remove duplicate `#[test]`)

**Step 1: Find and remove the duplicate #[test] attribute**

```bash
grep -n '#\[test\]' src/domain.rs
```

One test function will have `#[test]` twice. Remove the duplicate.

**Step 2: Run tests**

```bash
cargo test --bin quorum -- domain
```

Expected: PASS

**Step 3: Commit**

```bash
git add src/domain.rs
git commit -m "chore: remove duplicate #[test] attribute in domain.rs"
```

---

### Task 5: Fix `only_used_in_recursion` — analysis.rs

**Files:**
- Modify: `src/analysis.rs:~110`

**Step 1: Examine the flagged parameter**

```bash
cargo clippy --workspace --all-targets -- -D clippy::only_used_in_recursion 2>&1 | head -20
```

The parameter is only passed through unchanged to recursive calls. Either:
- (a) Remove the parameter if it's truly unused at the base case
- (b) If the parameter IS needed at the base case but clippy can't see it, add `#[allow(clippy::only_used_in_recursion)]` with a justification comment

**Step 2: Run tests**

```bash
cargo test --bin quorum -- analysis
```

**Step 3: Commit**

```bash
git add src/analysis.rs
git commit -m "chore: fix only_used_in_recursion lint in analysis.rs"
```

---

### Task 6: Triage `too_many_arguments` (3 functions)

**Files:**
- Modify: up to 3 source files

**Step 1: Identify the functions**

```bash
cargo clippy --workspace --all-targets -- -D clippy::too_many_arguments 2>&1
```

**Step 2: For each function, decide:**
- If the arguments can be grouped into a config/options struct → refactor
- If the function is internal and the signature is clear → `#[allow(clippy::too_many_arguments)]` with brief justification (e.g. "pipeline entry point, params are independent dimensions")
- If it's a test helper → suppress

**Step 3: Run tests**

```bash
cargo test --bin quorum
```

**Step 4: Commit**

```bash
git commit -m "chore: triage too_many_arguments lints (3 functions)"
```

---

### Task 7: Fix `type_complexity`

**Files:**
- Modify: 1 source file (identified by clippy output)

**Step 1: Identify the complex type**

```bash
cargo clippy --workspace --all-targets -- -D clippy::type_complexity 2>&1
```

**Step 2: Extract a type alias if the type is used more than once, or suppress with justification if it's a one-off generic bound**

**Step 3: Run tests and commit**

---

### Task 8: Final verification and CI update

**Files:**
- Modify: `.github/workflows/ci.yml` (remove `continue-on-error: true` from clippy job)

**Step 1: Run full clippy check — must be zero errors**

```bash
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Expected: no errors, no warnings.

**Step 2: Run full test suite**

```bash
cargo test --bin quorum
```

Expected: all tests pass.

**Step 3: Remove continue-on-error from clippy job**

In `.github/workflows/ci.yml`, find the clippy job and remove `continue-on-error: true`.

**Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: make clippy job blocking (zero warnings achieved)

Closes #214"
```

---

## Notes

- The `cargo fmt` sweep (#216) is separate — this PR only fixes clippy lints
- All collapsible_if rewrites use let-chains (stable since Rust 1.87, our MSRV is 1.93)
- No `#[allow]` without inline justification per issue #214 acceptance criteria
- #182 is a duplicate of #214 — close both
