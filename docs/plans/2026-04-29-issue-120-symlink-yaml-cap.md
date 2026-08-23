# Issue #120: User Rules Trust Boundary — Symlink Reject + YAML Size Cap

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Close issue #120's two PAL/cross-model-corroborated findings on `src/ast_grep.rs::load_rules` — (a) symlink follow yields arbitrary-file-read primitive, (b) unbounded YAML rule parsing enables local DoS. Scope: minimum-viable defense (Option A from brainstorm) — no canonicalize, no rule-bomb cap.

**Architecture:** Four insertion points in `load_rules` (revised after codex-cli review flagged TOCTOU + top-level gap):
1. **NEW — Top-level rules-root check**: before `read_dir(rules_dir)`, `symlink_metadata` on `rules_dir` itself; skip if symlink. Closes the case where `~/.quorum/rules → /etc` slips past everything else.
2. After `lang_entry.path()` (~line 42): `symlink_metadata` check on the lang directory; skip if symlink.
3. **REVISED — Open-then-validate, not stat-then-open**: instead of `symlink_metadata` followed by `read_to_string` (TOCTOU window: attacker can swap the path between checks), use `OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(&rule_path)`, then validate via the OPENED handle (`file.metadata().file_type().is_file()` + `meta.len() <= MAX_RULE_FILE_BYTES`), then read from the handle with a defensive `.take(MAX_RULE_FILE_BYTES + 1)` bound. Eliminates the TOCTOU since the handle is bound to the inode that existed at open time.
4. Use the same `MAX_RULE_FILE_BYTES = 1 MiB` cap, validated against the opened handle's `len()`.

**Residual risk acknowledged**: directory-level TOCTOU (between `symlink_metadata(&lang_dir)` and `read_dir(&lang_dir)`) cannot be cleanly closed in stable Rust without `openat2(O_NOFOLLOW | O_DIRECTORY)` machinery. The directory case leaks much less (only directory entry names, not file contents), so the residual is acceptable. File-content exfiltration — the higher-leverage threat — is fully closed by the O_NOFOLLOW open.

Tests use `tempfile::tempdir` + `std::os::unix::fs::symlink` to construct adversarial layouts. CI runs on Linux + macOS so `unix::fs::symlink` is fine; we don't support Windows so no `cfg` branching needed.

**Tech Stack:** Rust 2021, `std::fs::symlink_metadata`, `std::os::unix::fs::symlink`, `tempfile` (already a dev-dep), `tracing::warn!` for skip events.

---

## Scope

**IN scope:**
- 3-point fix in `load_rules`
- 4 new tests (symlinked dir skipped, symlinked file skipped, oversized file skipped, bundled rules still load)
- `MAX_RULE_FILE_BYTES` constant (1 MiB)
- `tracing::warn!` events on each skip with `path` field for observability
- CHANGELOG entry under `[Unreleased] Security`

**OUT of scope (deferred):**
- Path canonicalization + `starts_with(rules_dir)` bound check (Option B; not corroborated as required)
- `MAX_RULES_TOTAL` rule-bomb cap (Option C; theoretical, file as follow-up if observed in the wild)
- Restructuring `load_rules` itself (calibrator complaint at CC=11; pre-existing, out of scope)
- Windows symlink handling — Quorum doesn't support Windows

## Threat model

| Attack | Pre-fix | Post-fix |
|---|---|---|
| `~/.quorum/rules/python -> /etc/ssh/` | reads /etc/ssh/* as YAML rules | symlink_metadata returns is_symlink=true; skip with warn |
| `~/.quorum/rules/python/exfil.yml -> /etc/shadow` | read_to_string follows symlink, reads shadow | symlink_metadata on the file rejects symlinks |
| `~/.quorum/rules/python/huge.yml` (1 GB file) | unbounded allocation, hang/OOM | metadata().len() > 1 MiB; skip with warn |
| `~/.quorum/rules/python/dev_zero -> /dev/zero` | read_to_string blocks forever | symlink_metadata + is_file() rejects (target is char device, not regular file) |

The single-user-dev-machine framing of the original 2026-04-14 FP is wrong: writing to `~/.quorum/rules/` does not require an attacker to "have already won." Plenty of legitimate processes (npm install hooks, IDE plugins, malicious browser-extension installers) write to `~/`. Avoiding handing them an arbitrary-file-read primitive is the right defensive posture per the new precedence rule in #118.

---

## Implementation tasks (TDD order)

### Task 1: RED test — symlinked lang directory is skipped

**Files:**
- Modify: `src/ast_grep.rs` (add test in the `#[cfg(test)] mod tests` block)

**Step 1: Write the failing test**

Find the existing `mod tests` block at the bottom of `src/ast_grep.rs` (used by existing `all_bundled_rules_match_fixtures` etc). Add:

```rust
#[test]
#[cfg(unix)]
fn load_rules_skips_symlinked_lang_directory() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let project = tempdir().expect("project tempdir");
    let home = tempdir().expect("home tempdir");

    // Place a real bundled rule the loader will accept (so the test fails
    // for the symlink reason, not because nothing loads at all).
    let bundled_lang = project.path().join("rules").join("python");
    std::fs::create_dir_all(&bundled_lang).unwrap();
    std::fs::write(
        bundled_lang.join("safe.yml"),
        "id: safe-rule\nmessage: safe\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
    ).unwrap();

    // Adversarial: ~/.quorum/rules/python -> /etc/ (symlinked lang dir
    // pointing at an unrelated tree). Loader must NOT descend into it.
    let user_rules = home.path().join(".quorum").join("rules");
    std::fs::create_dir_all(&user_rules).unwrap();
    let evil_target = home.path().join("evil_target");
    std::fs::create_dir_all(&evil_target).unwrap();
    // Place a yml in the target so a symlink-follow loader would try to read it.
    std::fs::write(
        evil_target.join("evil.yml"),
        "id: evil-rule\nmessage: evil\nseverity: warning\nlanguage: python\nrule:\n  pattern: open($X)\n",
    ).unwrap();
    symlink(&evil_target, user_rules.join("python")).expect("symlink");

    let rules = crate::ast_grep::load_rules(project.path(), home.path());
    let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
    assert!(ids.contains(&"safe-rule".to_string()), "bundled rule should still load");
    assert!(
        !ids.contains(&"evil-rule".to_string()),
        "rule loaded from symlinked lang directory must be rejected; ids={ids:?}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum load_rules_skips_symlinked_lang_directory -- --nocapture`
Expected: FAIL — "rule loaded from symlinked lang directory must be rejected; ids=[..., \"evil-rule\"]"

**Step 3: Commit RED**

```bash
git add src/ast_grep.rs
git commit -m "test(ast_grep): RED — symlinked lang directory must be skipped

Refs #120"
```

### Task 2: GREEN — add `symlink_metadata` check on lang directory

**Step 1: Edit `load_rules`**

Find:
```rust
for lang_entry in lang_entries.flatten() {
    let lang_dir = lang_entry.path();
    if !lang_dir.is_dir() {
        continue;
    }
```

Replace with:
```rust
for lang_entry in lang_entries.flatten() {
    let lang_dir = lang_entry.path();
    // Reject symlinked lang dirs: symlink_metadata does NOT follow symlinks,
    // unlike is_dir() / metadata(). Issue #120: ~/.quorum/rules/<lang> being
    // a symlink to /etc/ would let read_to_string exfiltrate arbitrary files
    // under that target tree.
    let lang_meta = match std::fs::symlink_metadata(&lang_dir) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %lang_dir.display(), error = %e, "ast-grep: failed to stat lang dir; skipping");
            continue;
        }
    };
    if lang_meta.file_type().is_symlink() {
        tracing::warn!(path = %lang_dir.display(), "ast-grep: skipping symlinked lang directory");
        continue;
    }
    if !lang_meta.file_type().is_dir() {
        continue;
    }
```

**Step 2: Run test**

Run: `cargo test --bin quorum load_rules_skips_symlinked_lang_directory -- --nocapture`
Expected: PASS.

**Step 3: Run all ast_grep tests to ensure no regression**

Run: `cargo test --bin quorum ast_grep -- --nocapture`
Expected: All pass (existing fixture tests in `rules/<lang>/tests/` still load via the bundled path).

**Step 4: Commit GREEN**

```bash
git add src/ast_grep.rs
git commit -m "feat(ast_grep): reject symlinked lang directories in user rules tree

Closes one of two #120 paths: ~/.quorum/rules/<lang> being a symlink
to an arbitrary tree no longer causes read_to_string to follow into
attacker-controlled territory.

Uses symlink_metadata (does NOT follow) instead of is_dir() (follows).

Refs #120"
```

### Task 3: RED test — symlinked rule file is skipped

**Step 1: Write the failing test**

```rust
#[test]
#[cfg(unix)]
fn load_rules_skips_symlinked_rule_file() {
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let project = tempdir().expect("project tempdir");
    let home = tempdir().expect("home tempdir");

    // No bundled rules; only user rules.
    let user_python = home.path().join(".quorum").join("rules").join("python");
    std::fs::create_dir_all(&user_python).unwrap();

    // Real rule directly under user dir — must load.
    std::fs::write(
        user_python.join("real.yml"),
        "id: real-rule\nmessage: real\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
    ).unwrap();

    // Symlinked rule file pointing at content outside the rules tree.
    let outside = home.path().join("outside.yml");
    std::fs::write(
        &outside,
        "id: smuggled-rule\nmessage: smuggled\nseverity: warning\nlanguage: python\nrule:\n  pattern: eval($X)\n",
    ).unwrap();
    symlink(&outside, user_python.join("smuggled.yml")).expect("symlink");

    let rules = crate::ast_grep::load_rules(project.path(), home.path());
    let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
    assert!(ids.contains(&"real-rule".to_string()), "real rule should load; ids={ids:?}");
    assert!(
        !ids.contains(&"smuggled-rule".to_string()),
        "symlinked rule file must be rejected; ids={ids:?}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum load_rules_skips_symlinked_rule_file -- --nocapture`
Expected: FAIL — "symlinked rule file must be rejected; ids=[..., \"smuggled-rule\"]"

**Step 3: Commit RED**

```bash
git add src/ast_grep.rs
git commit -m "test(ast_grep): RED — symlinked rule file must be skipped

Refs #120"
```

### Task 4: GREEN — add `symlink_metadata` + `is_file()` check on rule files

**Step 1: Edit `load_rules`**

Find:
```rust
for rule_path in rule_files {
    let yaml = match std::fs::read_to_string(&rule_path) {
```

Replace with:
```rust
for rule_path in rule_files {
    // Reject symlinks and non-regular files (devices, sockets, FIFOs).
    // Same threat model as the lang-dir check: a rule file at
    // ~/.quorum/rules/<lang>/x.yml that's a symlink to /etc/shadow would
    // exfiltrate that file's content into the LLM context. read_to_string
    // also blocks indefinitely on FIFOs / /dev/zero.
    let rule_meta = match std::fs::symlink_metadata(&rule_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(path = %rule_path.display(), error = %e, "ast-grep: failed to stat rule file; skipping");
            continue;
        }
    };
    if !rule_meta.file_type().is_file() {
        tracing::warn!(path = %rule_path.display(), "ast-grep: skipping non-regular rule file (symlink, device, socket, or FIFO)");
        continue;
    }

    let yaml = match std::fs::read_to_string(&rule_path) {
```

Note: `is_file()` on `FileType` returns false for symlinks AND for non-regular types — single check covers both threats.

**Step 2: Run test**

Run: `cargo test --bin quorum load_rules_skips_symlinked_rule_file -- --nocapture`
Expected: PASS.

**Step 3: Re-run all ast_grep tests**

Run: `cargo test --bin quorum ast_grep -- --nocapture`
Expected: all pass.

**Step 4: Commit GREEN**

```bash
git add src/ast_grep.rs
git commit -m "feat(ast_grep): reject symlinked or non-regular rule files

Closes the second symlink path of #120: per-file symlink check
(symlink_metadata + is_file()) prevents read_to_string from
following ~/.quorum/rules/<lang>/x.yml into arbitrary target files,
including blocking on /dev/zero or FIFOs.

Refs #120"
```

### Task 5: RED test — oversized rule file is skipped

**Step 1: Write the failing test**

```rust
#[test]
#[cfg(unix)]
fn load_rules_skips_oversized_rule_file() {
    use tempfile::tempdir;

    let project = tempdir().expect("project tempdir");
    let home = tempdir().expect("home tempdir");

    let user_python = home.path().join(".quorum").join("rules").join("python");
    std::fs::create_dir_all(&user_python).unwrap();

    // Small, well-formed rule that must load.
    std::fs::write(
        user_python.join("small.yml"),
        "id: small-rule\nmessage: small\nseverity: warning\nlanguage: python\nrule:\n  pattern: print($X)\n",
    ).unwrap();

    // 2 MiB padded YAML — over the 1 MiB cap. Constructed so that IF the
    // size check is removed, the YAML still parses (otherwise we couldn't
    // distinguish "skipped due to size" from "skipped due to parse error").
    let prefix = "id: oversized-rule\nmessage: huge\nseverity: warning\nlanguage: python\nrule:\n  pattern: open($X)\ndescription: |\n";
    let padding = "x".repeat(2 * 1024 * 1024);
    let oversized = format!("{prefix}  {padding}\n");
    std::fs::write(user_python.join("oversized.yml"), oversized).unwrap();

    let rules = crate::ast_grep::load_rules(project.path(), home.path());
    let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
    assert!(ids.contains(&"small-rule".to_string()), "small rule should load; ids={ids:?}");
    assert!(
        !ids.contains(&"oversized-rule".to_string()),
        "rule file >1 MiB must be skipped; ids={ids:?}"
    );
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum load_rules_skips_oversized_rule_file -- --nocapture`
Expected: FAIL — "rule file >1 MiB must be skipped; ids=[..., \"oversized-rule\"]" (because the 2 MiB file is currently read+parsed unbounded).

**Step 3: Commit RED**

```bash
git add src/ast_grep.rs
git commit -m "test(ast_grep): RED — rule files >1 MiB must be skipped

Refs #120"
```

### Task 6: GREEN — add size cap before `read_to_string`

**Step 1: Add the constant + size check**

Near the top of `src/ast_grep.rs` (after the `use` block, before `ext_to_language`):

```rust
/// Maximum size for a single ast-grep YAML rule file. Files exceeding this
/// are skipped with a warning instead of being read into memory. Intended
/// to prevent DoS from oversized files in the user-rules tree
/// (~/.quorum/rules/<lang>/), where the trust boundary is weaker than the
/// bundled rules tree. See issue #120.
const MAX_RULE_FILE_BYTES: u64 = 1024 * 1024; // 1 MiB
```

**Step 2: Insert the size check right after the `is_file()` check** (Task 4's edit) and BEFORE `read_to_string`:

```rust
if rule_meta.len() > MAX_RULE_FILE_BYTES {
    tracing::warn!(
        path = %rule_path.display(),
        size = rule_meta.len(),
        cap = MAX_RULE_FILE_BYTES,
        "ast-grep: skipping rule file over size cap"
    );
    continue;
}
```

Note: we already have `rule_meta` from the `symlink_metadata` call — no second stat. `Metadata::len()` works for both `metadata()` and `symlink_metadata()` for regular files (returns the file's actual size, not the symlink's, but symlinks were rejected one check earlier so we're guaranteed regular file here).

**Step 3: Run test**

Run: `cargo test --bin quorum load_rules_skips_oversized_rule_file -- --nocapture`
Expected: PASS.

**Step 4: Re-run all ast_grep tests**

Run: `cargo test --bin quorum ast_grep -- --nocapture`
Expected: all pass; bundled rules under 1 MiB still load.

**Step 5: Commit GREEN**

```bash
git add src/ast_grep.rs
git commit -m "feat(ast_grep): cap rule files at 1 MiB before read_to_string

Closes the second half of #120: a >1 MiB rule file in
~/.quorum/rules/<lang>/ is no longer read into memory unbounded.
DoS via oversized YAML — billion-laughs variant, deeply nested
structures, or just a large file — now skipped with a warn.

Cap is intentionally generous (real rules are <10 KiB) so this
doesn't bite legitimate usage; intent is to bound worst case, not
enforce style.

Refs #120"
```

### Task 7: Defensive guard test — bundled rules still load post-fix

**Step 1: Write the test**

```rust
#[test]
fn load_rules_still_loads_bundled_python_rules_after_fix() {
    // Lightweight regression check: the symlink + size guards added for
    // #120 must NOT break the bundled-rules path. We invoke load_rules
    // against the actual repo's rules/ directory and assert at least one
    // python rule loads.
    let project_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let empty_home = tempfile::tempdir().expect("empty home for test");
    let rules = crate::ast_grep::load_rules(project_dir, empty_home.path());
    assert!(
        !rules.is_empty(),
        "bundled rules must still load after #120 hardening"
    );
    // Sanity: at least one of the well-known bundled rule IDs is present.
    let ids: Vec<_> = rules.iter().map(|r| r.id.clone()).collect();
    let has_known = ids.iter().any(|id| id.starts_with("md5") || id.starts_with("eval-") || id.starts_with("subprocess"));
    assert!(has_known, "expected a known bundled rule id; got {ids:?}");
}
```

**Step 2: Run**

Run: `cargo test --bin quorum load_rules_still_loads_bundled_python_rules_after_fix -- --nocapture`
Expected: PASS (green from the start — bundled rules are not symlinked, not >1 MiB).

**Step 3: Commit**

```bash
git add src/ast_grep.rs
git commit -m "test(ast_grep): regression guard — bundled rules still load after #120 hardening

Refs #120"
```

### Task 8: Verification gate

**Step 1: Full test suite**

Run: `cargo test 2>&1 | tail -10`
Expected: 1531 + 4 new = 1535 unit tests passing (plus integration suites).

**Step 2: Clippy on touched code**

Run: `cargo clippy --bin quorum 2>&1 | grep -E "ast_grep\.rs:[1-9]"`
Expected: no NEW warnings on the lines touched (lines ~1-100). Pre-existing CC=11 warning on `load_rules` is OK.

**Step 3: Release build**

Run: `cargo build --release 2>&1 | tail -5`
Expected: clean.

### Task 9: CHANGELOG entry

**Step 1: Edit `CHANGELOG.md`** — extend the existing `[Unreleased]` block:

```markdown
- **User rule loader hardened against symlink-follow + unbounded YAML (#120).** Pre-fix `load_rules` used `is_dir()` and `read_to_string()` which both follow symlinks; a symlink at `~/.quorum/rules/<lang>` or under it would let the loader read arbitrary files from any target the user has read access to (e.g. `~/.quorum/rules/python -> /etc/ssh/`), and `read_to_string()` had no size cap so a multi-MB or pathological YAML file could exhaust memory. Fix: switched to `symlink_metadata()` + `is_file()` on both lang-dir entries and rule-file entries (rejects symlinks, devices, sockets, FIFOs); added a 1 MiB cap via the same metadata stat before `read_to_string`. Skip events emit `tracing::warn!` for observability. Cross-model corroborated by 2026-04-28 PAL comparison (gpt-5.4 HIGH, claude-opus-4.5 MEDIUM); 2026-04-14 trust-model FP precedents that previously suppressed this finding class were overturned via Option-A Human TPs (entries #2240/#2241).
```

**Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): add #120 entry"
```

### Task 10: Quorum self-review

```bash
quorum review src/ast_grep.rs --no-color --parallel 4 2>&1 | tail -50
```

Triage findings:
- **In-branch bugs** — fix via TDD micro-cycle.
- **Pre-existing bugs** — file as separate GH issues; do NOT fix in this branch.
- Specifically expect the calibrator to hit the 2026-04-29 external TPs from the PAL run; the symlink/YAML-DoS classes should now CONFIRM since we just fixed them.

### Task 11: Record post_fix verdicts

For each finding now fixed, record `tp` with provenance `post_fix` (1.5x weight) so future calibrator runs respect the fix:

```bash
quorum feedback --file src/ast_grep.rs \
  --finding "User rule loader follows symlinks; arbitrary file read primitive (HIGH)" \
  --verdict tp --provenance post_fix \
  --reason "Fixed in #120 commits: symlink_metadata + is_file() rejects symlinked lang dirs and rule files; tracing::warn! on skip."

quorum feedback --file src/ast_grep.rs \
  --finding "Unbounded YAML rule parsing allows local DoS via oversized configs" \
  --verdict tp --provenance post_fix \
  --reason "Fixed in #120 commits: 1 MiB MAX_RULE_FILE_BYTES cap via metadata().len() check before read_to_string."
```

(Also flips the previously-overturned 2026-04-14 FPs onto firmer footing — combined with the post_fix entries at 1.5x, the corpus now has unambiguous TP signal for these patterns.)

### Task 12: Independent review + finishing

Use `superpowers:requesting-code-review` for an independent review pass on the diff before merge.

Then `superpowers:finishing-a-development-branch` for the merge/PR/cleanup choice.

---

## Acceptance checklist

- [ ] `symlink_metadata` rejects symlinked lang directories
- [ ] `symlink_metadata` + `is_file()` rejects symlinked, device, socket, FIFO rule files
- [ ] `metadata().len()` size cap (1 MiB) before `read_to_string`
- [ ] 4 new tests: symlink-dir-skipped, symlink-file-skipped, oversized-skipped, bundled-still-loads
- [ ] Single shared `symlink_metadata` call (no duplicate stats)
- [ ] `tracing::warn!` events on each skip with `path` field
- [ ] CHANGELOG entry under `[Unreleased] Fixed`
- [ ] `cargo test --bin quorum` passes (baseline + 4)
- [ ] `cargo clippy --bin quorum` clean on touched lines
- [ ] Quorum self-review (Task 10) — findings triaged
- [ ] `post_fix` feedback verdicts for both PAL findings (Task 11)
- [ ] Issue #120 closed with reference to validation results

## Risk register

| Risk | Mitigation |
|---|---|
| Tests rely on `unix::fs::symlink` — Windows breaks | `#[cfg(unix)]` gate; we don't ship for Windows |
| `symlink_metadata` errors on permission-denied entries cause silent skips | tracing::warn! with `error = %e` makes these observable |
| Bundled rules > 1 MiB get skipped silently | Largest bundled rule today is <10 KiB; 1 MiB cap is 100× headroom |
| Existing 2026-04-14 trust-model FP precedents still anti-anchor on next review | Already overturned via Option-A Human TPs (entries #2240/#2241); post_fix entries at 1.5x further dominate. #122 supersession lands later. |
