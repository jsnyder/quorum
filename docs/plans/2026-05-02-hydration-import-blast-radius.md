# Hydration Import & Blast Radius Fixes (#178, #179)

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix two bugs in `src/hydration.rs` — overly broad caller blast radius trigger and incorrect Python import local-name extraction.

**Architecture:** Both fixes are surgical edits to existing functions in `hydration.rs`. No new files, no new dependencies. Existing test infrastructure covers both paths.

**Tech Stack:** Rust, tree-sitter

---

## Task 1: Fix caller blast radius trigger (#178)

**Files:**
- Modify: `src/hydration.rs:74-80` (the caller blast-radius loop)

**Context:** The loop at line 74 triggers `find_callers_of` when any line in a function definition (`fstart..=fend`) overlaps the changed region. The intent is to find callers only when the function's *signature* changes, not body-only edits. The tuple is `(name, signature, fstart, fend)` where `fstart` is the first line of the function definition (the signature line).

**Step 1: Write the failing test**

Add a test that creates a function with a multi-line body, marks only body lines as changed, and asserts `ctx.callers` is empty:

```rust
#[test]
fn caller_blast_radius_ignores_body_only_edits() {
    let code = r#"
fn helper() -> i32 {
    42
}

fn caller() {
    helper();
}
"#;
    // Changed lines cover only the body of `helper` (line 3), not the signature (line 2)
    let ctx = hydrate(code, Language::Rust, &[(3, 3)]);
    assert!(ctx.callers.is_empty(), "body-only edit should not trigger caller search");
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum caller_blast_radius_ignores_body_only_edits`
Expected: FAIL — callers will be populated because the current condition matches any overlap.

**Step 3: Write minimal fix**

Change line 76 from:
```rust
if *fstart <= end && *fend >= start {
```
to:
```rust
if *fstart >= start && *fstart <= end {
```

This restricts the trigger to cases where the function's signature line (`fstart`) is within the changed range.

**Step 4: Run test to verify it passes**

Run: `cargo test --bin quorum caller_blast_radius_ignores_body_only_edits`
Expected: PASS

**Step 5: Add a positive test — signature edit DOES trigger blast radius**

```rust
#[test]
fn caller_blast_radius_triggers_on_signature_edit() {
    let code = r#"
fn helper() -> i32 {
    42
}

fn caller() {
    helper();
}
"#;
    // Changed line covers the signature of `helper` (line 2)
    let ctx = hydrate(code, Language::Rust, &[(2, 2)]);
    assert!(!ctx.callers.is_empty(), "signature edit should trigger caller search");
}
```

Run: `cargo test --bin quorum caller_blast_radius_triggers_on_signature`
Expected: PASS (existing behavior preserved)

**Step 6: Commit**

```bash
git add src/hydration.rs
git commit -m "fix(hydration): narrow caller blast radius to signature-line changes (#178)"
```

---

## Task 2: Fix Python `from X import foo as bar` returning wrong name (#179a)

**Files:**
- Modify: `src/hydration.rs:239-245` (Python `from X import ...` branch)

**Context:** Line 241 uses `.split(" as ").next()` which returns the part *before* `as` (the module name). Should return the part *after* `as` (the local binding). Also, parenthesized imports like `from x import (a, b, c)` need paren stripping.

**Step 1: Write the failing test**

```rust
#[test]
fn python_from_import_as_returns_local_binding() {
    let names = extract_imported_names("from os.path import join as pjoin, exists", Language::Python);
    assert_eq!(names, vec!["pjoin", "exists"]);
}

#[test]
fn python_from_import_parenthesized() {
    let names = extract_imported_names("from os import (path, getcwd, listdir)", Language::Python);
    assert_eq!(names, vec!["path", "getcwd", "listdir"]);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum python_from_import`
Expected: FAIL — first test returns `["join", "exists"]` instead of `["pjoin", "exists"]`.

**Step 3: Write minimal fix**

Replace lines 239-245 with:

```rust
if let Some(after_import) = text.split("import").nth(1) {
    // Strip parentheses for `from x import (a, b, c)` form
    let cleaned = after_import.trim().trim_start_matches('(').trim_end_matches(')');
    for part in cleaned.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // `foo as bar` -> local binding is `bar` (after `as`)
        let name = if let Some(after_as) = part.split(" as ").nth(1) {
            after_as.trim()
        } else {
            part
        };
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum python_from_import`
Expected: PASS

**Step 5: Commit**

```bash
git add src/hydration.rs
git commit -m "fix(hydration): Python from-import returns local binding after as (#179)"
```

---

## Task 3: Fix Python `import foo.bar as baz` returning wrong name (#179b)

**Files:**
- Modify: `src/hydration.rs:312-317` (Python `import X` branch, non-TS path)

**Context:** Line 313-314 does `module.split('.').last()` without checking for `as` aliasing. `import foo.bar as baz` should yield `baz`, not `bar`.

**Step 1: Write the failing test**

```rust
#[test]
fn python_import_as_returns_alias() {
    let names = extract_imported_names("import foo.bar as baz", Language::Python);
    assert_eq!(names, vec!["baz"]);
}

#[test]
fn python_import_dotted_no_alias() {
    let names = extract_imported_names("import os.path", Language::Python);
    assert_eq!(names, vec!["path"]);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum python_import_as_returns_alias`
Expected: FAIL — returns `["bar"]` instead of `["baz"]`.

**Step 3: Write minimal fix**

Replace lines 312-317 with:

```rust
// Python: import sys / import foo.bar / import foo.bar as baz
let module = text.trim_start_matches("import ").trim();
let name = if let Some(after_as) = module.split(" as ").nth(1) {
    after_as.trim()
} else {
    module.split('.').last().unwrap_or(module).trim()
};
if !name.is_empty() {
    names.push(name.to_string());
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --bin quorum python_import_as`
Expected: PASS

**Step 5: Commit**

```bash
git add src/hydration.rs
git commit -m "fix(hydration): Python import-as returns alias, not dotted segment (#179)"
```

---

## Task 4: Verification

Run full test suite, clippy, and release build:

```bash
cargo test --bin quorum
cargo clippy --bin quorum -- -D warnings
cargo build --release
```

Expected: All tests pass, zero clippy warnings, clean release build.
