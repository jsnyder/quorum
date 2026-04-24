# Issue #29 Follow-up Test Plan

Date: 2026-04-24
Branch: fix/issue-29-followups
Run: `cargo test --bin quorum`

Tests are inline `#[cfg(test)] mod tests` blocks in the same file as the function under test, matching existing layout (`requirements_txt_*`, `pyproject_*`, `normalize_*`).

---

## Bug 1: `parse_python_import` drops trailing modules

File: `src/context_enrichment.rs`
Current: `stmt.split_whitespace().next()` returns only the first module.
Production input arrives via `normalize_import_to_dep_names` as `"{symbol}: import <body>"`.

### RED tests (place near `normalize_module_path_returns_root_segment`)

```rust
#[test]
fn parse_python_import_returns_all_modules_in_comma_list() {
    // RED: today returns only ["os"]; "sys" and "json" are dropped.
    assert_eq!(
        normalize_import_to_dep_names("join: import os, sys, json"),
        vec!["os", "sys", "json"]
    );
}

#[test]
fn parse_python_import_strips_as_alias() {
    // "import sys as s" -> root segment "sys", alias discarded.
    assert_eq!(
        normalize_import_to_dep_names("s: import sys as s"),
        vec!["sys"]
    );
}

#[test]
fn parse_python_import_dotted_returns_root_only_per_module() {
    // "import os.path, urllib.parse as up" -> ["os", "urllib"].
    // Regression guard: dotted submodule must collapse to root,
    // alias must not leak in as a fake module.
    assert_eq!(
        normalize_import_to_dep_names("path: import os.path, urllib.parse as up"),
        vec!["os", "urllib"]
    );
}
```

### Edge cases / regression guards
- Whitespace: `"import   os ,  sys"` -> `["os","sys"]` (split on `,`, then trim).
- Inline comment tail: `"import os  # bootstrap"` -> `["os"]` (split off `#` before comma split).
- Trailing comma / empty segment: `"import os,"` -> `["os"]` (skip empties; do not push `""`).
- Empty after `import `: `"import "` -> `vec![]` (no panic, no `""` head).
- Already-covered single-module case (`"import sys"` -> `["sys"]`) must keep passing.

### Expected post-fix
`parse_python_import` splits on `,`, trims each segment, drops `as <alias>`, takes the first dotted segment, filters empties.

---

## Bug 2: Empty `pyproject.toml` suppresses `requirements.txt` fallback

File: `src/dep_manifest.rs`
Current: `parse_dependencies` only consults `requirements.txt` when `pyproject.toml` is absent. A pyproject that yields `vec![]` (parse error, missing both `[project]` and `[tool.poetry]`) silently returns no deps.

The fix must distinguish "pyproject declared zero deps" (e.g. `dependencies = []`) from "pyproject yielded nothing useful" (no recognized section). Suggest `parse_pyproject` returning `Option<Vec<Dependency>>` (`None` = no recognized section, `Some(vec![])` = explicit empty), or a sibling `pyproject_has_dep_section(&Path) -> bool`.

### RED tests (place near `pyproject_empty_pep621_array_wins_over_poetry`)

```rust
#[test]
fn pyproject_without_known_sections_falls_through_to_requirements() {
    // RED: today pyproject.exists() short-circuits, so requirements.txt is ignored.
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", "[build-system]\nrequires = [\"setuptools\"]\n");
    write(dir.path(), "requirements.txt", "fastapi\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}

#[test]
fn pyproject_unparseable_falls_through_to_requirements() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "pyproject.toml", "this is not valid toml ===\n");
    write(dir.path(), "requirements.txt", "django\n");
    let names: Vec<_> = parse_dependencies(dir.path()).iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["django"]);
}
```

### Regression guards (must keep passing)
- `pyproject_empty_pep621_array_wins_over_poetry` (explicit `dependencies = []` does NOT fall through).
- `requirements_txt_skipped_when_pyproject_present` (real PEP 621 deps still beat requirements.txt).

---

## Bug 3: PEP 508 named direct-URL refs dropped

File: `src/dep_manifest.rs`, `parse_requirements_txt`
Current: any line containing `://` or starting with `git+` is dropped, losing the usable name in `mypkg @ git+https://...`.

### RED tests (extend `requirements_txt_skips_vcs_urls` cluster)

```rust
#[test]
fn requirements_txt_keeps_pep508_named_git_url() {
    // RED: "mypkg @ git+https://..." is dropped today; should keep "mypkg".
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt",
        "fastapi\nmypkg @ git+https://github.com/foo/bar.git\n");
    let mut names: Vec<_> = parse_dependencies(dir.path())
        .iter().map(|d| d.name.clone()).collect();
    names.sort();
    assert_eq!(names, vec!["fastapi", "mypkg"]);
}

#[test]
fn requirements_txt_keeps_pep508_named_https_wheel() {
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt",
        "wheelpkg @ https://example.com/pkg.whl\n");
    let names: Vec<_> = parse_dependencies(dir.path())
        .iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["wheelpkg"]);
}

#[test]
fn requirements_txt_still_skips_unnamed_vcs_urls() {
    // Regression guard: bare "git+https://..." with no "name @" prefix
    // must still be skipped (no valid dep name to extract).
    let dir = TempDir::new().unwrap();
    write(dir.path(), "requirements.txt",
        "fastapi\ngit+https://github.com/x/y.git\nhttps://example.com/pkg.whl\n");
    let names: Vec<_> = parse_dependencies(dir.path())
        .iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, vec!["fastapi"]);
}
```

### Edge cases
- Whitespace variants: `"mypkg@git+https://..."` (no spaces) and `"mypkg  @  git+https://..."` both yield `"mypkg"`.
- Extras before `@`: `"mypkg[extra] @ git+https://..."` yields `"mypkg"` (drop extras, same as `strip_python_dep_spec`).
- Comment trailing the URL: `"mypkg @ git+https://x/y.git  # pinned"` yields `"mypkg"`.
- Empty name before `@`: `" @ git+https://..."` skipped (no name).

### Expected post-fix
Before the URL filter, if a line contains ` @ ` (or `@` with a non-empty bareword to its left), split on the first `@`, trim, run the LHS through `strip_python_dep_spec`, push if non-empty. Only fully unnamed URLs hit the existing skip branch. Mirror the same logic in `parse_pyproject` (covered by existing `pyproject_pep621_skips_pep508_direct_url_refs`, which should be upgraded to assert the named one is kept).
