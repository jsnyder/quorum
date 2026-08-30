# Incremental Indexing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Merge `index`/`refresh` into one smart command, add incremental (diff-based) indexing, and add progress output — making indexing fast enough for routine use.

**Architecture:** The unified `index` command defaults to incremental behavior (skip unchanged sources, only re-extract/re-embed changed files). A `--force` flag triggers full rebuild. Progress output uses counting-up numbers on stderr per DESIGN.md conventions. The `GitOps` trait gains a `diff_files` method, `IndexBuilder` gains a `update_files` method for surgical DB updates, and `extract_source` gains an optional file filter.

**Tech Stack:** Rust, clap (CLI), rusqlite + sqlite-vec (index DB), tree-sitter (AST extraction), walkdir (file discovery)

---

## File Structure

| File | Responsibility | Action |
|------|---------------|--------|
| `src/cli/mod.rs` | Clap definitions for context subcommands | Modify: remove `ContextRefreshOpts`, add `--force` to `ContextIndexOpts` |
| `src/main.rs` | Command dispatch | Modify: remove `Refresh` arm, add force flag mapping |
| `src/context/cli.rs` | Internal command types + handlers | Modify: remove `RefreshArgs`/`ContextCmd::Refresh`, add `force` to `IndexArgs`, merge refresh logic into index, add incremental path, add progress output |
| `src/context/inject/stale.rs` | Git operations trait | Modify: add `diff_files` method to `GitOps` trait + impls |
| `src/context/extract/dispatch.rs` | Source extraction | Modify: add optional `file_filter` param to `extract_source` |
| `src/context/index/builder.rs` | SQLite index builder | Modify: add `update_files` method for surgical chunk replacement |
| `src/context/index/state.rs` | Index state persistence | No changes needed |
| `src/context/cli_tests.rs` | Tests | Modify: update all refresh tests to use index with force flag, add incremental tests, add progress tests |

---

### Task 1: Add `diff_files` to GitOps trait

**Files:**
- Modify: `src/context/inject/stale.rs:33-138`
- Test: `src/context/cli_tests.rs` (new test)

- [ ] **Step 1: Write failing test for FakeGit::diff_files**

Add to `src/context/inject/stale.rs` after the existing `FakeGit` impl block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn fake_git_diff_files_returns_canned_list() {
        let mut git = FakeGit::default();
        let root = PathBuf::from("/repo");
        git.set_diff_files(&root, vec!["src/main.rs".into(), "src/lib.rs".into()]);
        let files = git.diff_files(&root, "abc123").unwrap();
        assert_eq!(files, Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]));
    }

    #[test]
    fn fake_git_diff_files_returns_none_when_not_set() {
        let git = FakeGit::default();
        let root = PathBuf::from("/repo");
        let files = git.diff_files(&root, "abc123").unwrap();
        assert_eq!(files, None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum -- stale::tests::fake_git_diff_files -v`
Expected: FAIL — `diff_files` method doesn't exist

- [ ] **Step 3: Add diff_files to GitOps trait and implementations**

In `src/context/inject/stale.rs`, add to the `GitOps` trait (after `head_sha`):

```rust
    /// List files changed between `from_sha` and current HEAD.
    /// Returns `Ok(None)` when the directory is not a git repo or
    /// `from_sha` is not a valid ancestor. Returns `Ok(Some(vec))` with
    /// relative paths (forward-slash separated) of changed files.
    fn diff_files(&self, repo_root: &Path, from_sha: &str) -> std::io::Result<Option<Vec<String>>>;
```

Add `SystemGit` implementation:

```rust
    fn diff_files(&self, repo_root: &Path, from_sha: &str) -> std::io::Result<Option<Vec<String>>> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("diff")
            .arg("--name-only")
            .arg(format!("{from_sha}..HEAD"))
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let files: Vec<String> = stdout
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.replace('\\', "/"))
            .collect();
        Ok(Some(files))
    }
```

Add `diff_by_path` field to `FakeGit`:

```rust
pub struct FakeGit {
    pub dirty: bool,
    pub head_by_path: std::collections::HashMap<std::path::PathBuf, Option<String>>,
    pub default_head: Option<String>,
    pub diff_by_path: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
}
```

Update `FakeGit::default()` to initialize `diff_by_path: HashMap::new()`.

Add `set_diff_files` method and `diff_files` impl:

```rust
    pub fn set_diff_files(&mut self, path: &Path, files: Vec<String>) {
        self.diff_by_path.insert(path.to_path_buf(), files);
    }

    // In GitOps impl:
    fn diff_files(&self, repo_root: &Path, _from_sha: &str) -> std::io::Result<Option<Vec<String>>> {
        Ok(self.diff_by_path.get(repo_root).cloned())
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test --bin quorum -- stale::tests -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/context/inject/stale.rs
rtk git commit -m "feat(context): add diff_files to GitOps trait (#361)"
```

---

### Task 2: Add file-filtered extraction

**Files:**
- Modify: `src/context/extract/dispatch.rs:30-55`
- Test: inline in the existing test patterns

- [ ] **Step 1: Write failing test**

Add to `src/context/extract/dispatch.rs` (at bottom, in a `#[cfg(test)] mod tests` block):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::{SourceEntry, SourceKind, SourceLocation};
    use crate::context::index::traits::FixedClock;
    use std::collections::HashSet;

    fn mini_rust_source() -> SourceEntry {
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/context/repos/mini-rust");
        SourceEntry {
            name: "test".into(),
            kind: SourceKind::Rust,
            location: SourceLocation::Path(fixture),
            weight: 1,
            ignore: vec![],
            paths: vec![],
        }
    }

    #[test]
    fn extract_with_file_filter_only_extracts_matching_files() {
        let source = mini_rust_source();
        let clock = FixedClock::epoch();
        let config = ExtractConfig::default();
        let filter: HashSet<String> = ["src/main.rs".to_string()].into_iter().collect();
        let result = extract_source_filtered(&source, &config, &clock, Some(&filter)).unwrap();
        // All chunks should come from the filtered file only
        for chunk in &result.chunks {
            assert_eq!(
                chunk.metadata.source_path, "src/main.rs",
                "chunk from wrong file: {:?}", chunk.metadata.source_path
            );
        }
        assert!(!result.chunks.is_empty(), "filter should still extract matching file");
    }

    #[test]
    fn extract_with_no_filter_extracts_all() {
        let source = mini_rust_source();
        let clock = FixedClock::epoch();
        let config = ExtractConfig::default();
        let unfiltered = extract_source_filtered(&source, &config, &clock, None).unwrap();
        let all = extract_source(&source, &config, &clock).unwrap();
        assert_eq!(unfiltered.chunks.len(), all.chunks.len());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum -- extract::dispatch::tests -v`
Expected: FAIL — `extract_source_filtered` doesn't exist

- [ ] **Step 3: Implement extract_source_filtered**

Add a new public function in `src/context/extract/dispatch.rs` right after `extract_source`:

```rust
/// Like `extract_source` but only processes files whose relative path is in
/// `file_filter`. When `file_filter` is `None`, extracts all files (identical
/// to `extract_source`).
pub fn extract_source_filtered(
    source: &SourceEntry,
    config: &ExtractConfig,
    clock: &dyn Clock,
    file_filter: Option<&std::collections::HashSet<String>>,
) -> anyhow::Result<ExtractResult> {
    extract_source_inner(source, config, clock, file_filter)
}
```

Then rename `extract_source` to delegate to `extract_source_inner`, and add the filter check right after the relative path is computed (after `diagnostics.total_files_scanned += 1`):

```rust
pub fn extract_source(
    source: &SourceEntry,
    config: &ExtractConfig,
    clock: &dyn Clock,
) -> anyhow::Result<ExtractResult> {
    extract_source_inner(source, config, clock, None)
}

fn extract_source_inner(
    source: &SourceEntry,
    config: &ExtractConfig,
    clock: &dyn Clock,
    file_filter: Option<&std::collections::HashSet<String>>,
) -> anyhow::Result<ExtractResult> {
    // ... existing body, with this added after total_files_scanned increment:
    
            // File filter: skip files not in the changed set.
            if let Some(filter) = file_filter {
                if !filter.contains(&rel) {
                    continue;
                }
            }
    // ... rest of function unchanged
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test --bin quorum -- extract::dispatch::tests -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/context/extract/dispatch.rs
rtk git commit -m "feat(context): add file-filtered extraction for incremental indexing (#361)"
```

---

### Task 3: Add `update_files` to IndexBuilder

**Files:**
- Modify: `src/context/index/builder.rs`
- Test: inline in builder module

- [ ] **Step 1: Write failing test**

Add to the existing test module in `src/context/index/builder.rs` (or create one at the bottom):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::index::traits::{FixedClock, HashEmbedder};
    use crate::context::types::*;
    use chrono::Utc;

    fn test_chunk(id: &str, source: &str, path: &str, content: &str) -> Chunk {
        Chunk {
            id: id.to_string(),
            source: source.to_string(),
            kind: ChunkKind::Symbol,
            subtype: None,
            qualified_name: Some(id.to_string()),
            signature: None,
            content: content.to_string(),
            metadata: ChunkMetadata {
                source_path: path.to_string(),
                line_range: 1..=10,
                commit_sha: "abc".to_string(),
                indexed_at: Utc::now(),
                source_version: None,
                language: Some("rust".to_string()),
                is_exported: false,
                neighboring_symbols: vec![],
            },
            provenance: ChunkProvenance::LocalAst,
        }
    }

    #[test]
    fn update_files_replaces_only_targeted_file_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("index.db");
        let clock = FixedClock::epoch();
        let embedder = HashEmbedder::new(384);

        let mut builder = IndexBuilder::new(&db_path, &clock, &embedder).unwrap();

        // Insert initial chunks from two files
        let chunks_a = vec![test_chunk("a1", "src", "src/a.rs", "fn a() {}")];
        let chunks_b = vec![test_chunk("b1", "src", "src/b.rs", "fn b() {}")];
        let all: Vec<_> = chunks_a.iter().chain(chunks_b.iter()).cloned().collect();
        builder.insert_chunks("src", &all).unwrap();

        // Now update only file a.rs with a new chunk
        let new_a = vec![test_chunk("a2", "src", "src/a.rs", "fn a_new() {}")];
        let changed_files: std::collections::HashSet<String> =
            ["src/a.rs".to_string()].into_iter().collect();
        let report = builder.update_files("src", &new_a, &changed_files, &std::collections::HashMap::new()).unwrap();

        assert_eq!(report.chunks_inserted, 1, "should insert 1 new chunk for a.rs");

        // b.rs chunk should still exist
        let count: i64 = builder.conn()
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE source_path = 'src/b.rs'",
                [], |r| r.get(0)
            ).unwrap();
        assert_eq!(count, 1, "b.rs chunk should be untouched");

        // Old a.rs chunk should be gone, new one present
        let a_count: i64 = builder.conn()
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE source_path = 'src/a.rs'",
                [], |r| r.get(0)
            ).unwrap();
        assert_eq!(a_count, 1, "should have exactly 1 a.rs chunk (the new one)");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum -- index::builder::tests::update_files -v`
Expected: FAIL — `update_files` and `insert_chunks` don't exist

- [ ] **Step 3: Implement insert_chunks and update_files**

Add to `IndexBuilder` impl in `src/context/index/builder.rs`:

```rust
    /// Insert chunks directly (without going through JSONL). Used by the
    /// incremental index path.
    pub fn insert_chunks(
        &mut self,
        source_name: &str,
        chunks: &[Chunk],
    ) -> anyhow::Result<usize> {
        let mut embedded: Vec<(&Chunk, Vec<f32>)> = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            if chunk.content.is_empty() {
                continue;
            }
            let vec = self.embedder.embed(&chunk.content);
            embedded.push((chunk, vec));
        }
        let tx = self.conn.transaction()?;
        Self::insert_embedded_chunks(&tx, &embedded)?;
        let count = embedded.len();
        tx.commit()?;
        Ok(count)
    }

    /// Surgical update: delete chunks belonging to `changed_files`, then insert
    /// `new_chunks` (which should come from re-extracting only those files).
    /// Chunks for files NOT in `changed_files` are untouched.
    pub fn update_files(
        &mut self,
        source_name: &str,
        new_chunks: &[Chunk],
        changed_files: &std::collections::HashSet<String>,
        fingerprints: &std::collections::HashMap<String, [f32; crate::context::extract::fingerprint::FINGERPRINT_DIMS]>,
    ) -> anyhow::Result<RebuildReport> {
        let mut report = RebuildReport {
            source: source_name.to_string(),
            chunks_loaded: new_chunks.len(),
            ..Default::default()
        };

        // Pre-embed new chunks
        let mut embedded: Vec<(&Chunk, Vec<f32>)> = Vec::with_capacity(new_chunks.len());
        for chunk in new_chunks {
            if chunk.content.is_empty() {
                continue;
            }
            let vec = self.embedder.embed(&chunk.content);
            embedded.push((chunk, vec));
        }
        report.chunks_embedded = embedded.len();

        let tx = self.conn.transaction()?;

        // Delete old chunks for changed files only
        let placeholders: String = changed_files
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(",");

        if !changed_files.is_empty() {
            let file_list: Vec<&str> = changed_files.iter().map(|s| s.as_str()).collect();

            let del_struct = format!(
                "DELETE FROM chunks_struct_vec WHERE chunk_id IN \
                 (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
            );
            let del_vec = format!(
                "DELETE FROM chunks_vec WHERE id IN \
                 (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
            );
            let del_fts = format!(
                "DELETE FROM chunks_fts WHERE id IN \
                 (SELECT id FROM chunks WHERE source = ?1 AND source_path IN ({placeholders}))"
            );
            let del_chunks = format!(
                "DELETE FROM chunks WHERE source = ?1 AND source_path IN ({placeholders})"
            );

            for sql in [&del_struct, &del_vec, &del_fts, &del_chunks] {
                let mut stmt = tx.prepare(sql)?;
                let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
                params_vec.push(Box::new(source_name.to_string()));
                for f in &file_list {
                    params_vec.push(Box::new(f.to_string()));
                }
                let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                    params_vec.iter().map(|b| b.as_ref()).collect();
                stmt.execute(param_refs.as_slice())?;
            }

            report.prior_source_chunks_removed = changed_files.len();
        }

        // Insert new chunks
        Self::insert_embedded_chunks(&tx, &embedded)?;

        // Insert fingerprints for new chunks
        if !fingerprints.is_empty() {
            let mut ins_fp = tx.prepare(
                "INSERT OR REPLACE INTO chunks_struct_vec(chunk_id, structural_vec) VALUES (?1, ?2)"
            )?;
            for (chunk_id, fp) in fingerprints {
                let bytes = f32_vec_to_le_bytes(fp);
                ins_fp.execute(params![chunk_id, bytes])?;
            }
        }

        report.chunks_inserted = embedded.len();
        tx.commit()?;
        Ok(report)
    }

    /// Shared insert logic used by both rebuild_from_jsonl and update_files.
    fn insert_embedded_chunks(
        tx: &rusqlite::Transaction,
        embedded: &[(&Chunk, Vec<f32>)],
    ) -> anyhow::Result<()> {
        let mut ins_chunk = tx.prepare(
            "INSERT INTO chunks (
                id, source, kind, subtype, qualified_name, signature, content,
                source_path, line_start, line_end, commit_sha, indexed_at,
                source_version, language, is_exported, neighboring_symbols,
                extractor, confidence, source_uri
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                ?13, ?14, ?15, ?16, ?17, ?18, ?19
            )",
        )?;
        let mut ins_fts = tx.prepare(
            "INSERT INTO chunks_fts (id, content, qualified_name, signature)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut ins_vec =
            tx.prepare("INSERT INTO chunks_vec(id, embedding) VALUES (?1, ?2)")?;

        for (chunk, vec) in embedded {
            let kind_str = serde_json::to_value(&chunk.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let neighbors_json = serde_json::to_string(&chunk.metadata.neighboring_symbols)?;
            let indexed_at = chunk.metadata.indexed_at.to_rfc3339();

            ins_chunk.execute(params![
                chunk.id,
                chunk.source,
                kind_str,
                chunk.subtype,
                chunk.qualified_name,
                chunk.signature,
                chunk.content,
                chunk.metadata.source_path,
                chunk.metadata.line_range.start(),
                chunk.metadata.line_range.end(),
                chunk.metadata.commit_sha,
                indexed_at,
                chunk.metadata.source_version,
                chunk.metadata.language,
                i32::from(chunk.metadata.is_exported),
                neighbors_json,
                chunk.provenance.extractor(),
                chunk.provenance.confidence(),
                chunk.provenance.source_uri(),
            ])?;

            ins_fts.execute(params![
                chunk.id,
                chunk.content,
                chunk.qualified_name.clone().unwrap_or_default(),
                chunk.signature.clone().unwrap_or_default(),
            ])?;

            let bytes = f32_vec_to_le_bytes(vec);
            ins_vec.execute(params![chunk.id, bytes])?;
        }
        Ok(())
    }
```

Also refactor `rebuild_from_jsonl` to use `insert_embedded_chunks` (DRY).

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test --bin quorum -- index::builder::tests -v`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/context/index/builder.rs
rtk git commit -m "feat(context): add update_files for surgical chunk replacement (#361)"
```

---

### Task 4: Merge index/refresh — remove Refresh command

**Files:**
- Modify: `src/cli/mod.rs:50-203`
- Modify: `src/main.rs:548-605`
- Modify: `src/context/cli.rs:348-431, 925-1161`
- Test: `src/context/cli_tests.rs`

- [ ] **Step 1: Write failing test — index with force:false skips unchanged sources**

In `src/context/cli_tests.rs`, add:

```rust
#[test]
fn index_skips_unchanged_source_when_not_forced() {
    let deps = TestDeps::new();
    seed_single_source(&deps, "mini", "mini-rust");

    // First index to lay down state.json
    let args = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: false,
    };
    run_context_cmd(&ContextCmd::Index(args.clone()), &deps).expect("first index");

    // Second index should skip (same HEAD sha)
    let out = run_context_cmd(&ContextCmd::Index(args), &deps).expect("second index");
    assert!(
        out.stdout.contains("skipped 'mini'"),
        "non-forced index must skip unchanged source: {:?}",
        out.stdout
    );
}

#[test]
fn index_force_rebuilds_even_when_unchanged() {
    let deps = TestDeps::new();
    seed_single_source(&deps, "mini", "mini-rust");

    let smart = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: false,
    };
    run_context_cmd(&ContextCmd::Index(smart), &deps).expect("first index");

    let forced = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: true,
    };
    let out = run_context_cmd(&ContextCmd::Index(forced), &deps).expect("forced index");
    assert!(
        out.stdout.contains("indexed 'mini'"),
        "forced index must rebuild: {:?}",
        out.stdout
    );
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `rtk cargo test --bin quorum -- index_skips_unchanged index_force_rebuilds -v`
Expected: FAIL — `IndexArgs` has no `force` field

- [ ] **Step 3: Add force field to IndexArgs**

In `src/context/cli.rs`, modify `IndexArgs`:

```rust
#[derive(Debug, Clone, Default)]
pub struct IndexArgs {
    pub selector: SourceSelector,
    pub force: bool,
}
```

Remove `RefreshArgs` struct entirely.

Remove `ContextCmd::Refresh` variant and its `name()` match arm.

- [ ] **Step 4: Merge refresh logic into index handler**

Replace `run_index` in `src/context/cli.rs` with unified logic:

```rust
fn run_index<D: ContextDeps>(args: &IndexArgs, deps: &D) -> Result<CmdOutput> {
    let cfg = load_sources_or_err(deps)?;
    let entries = selected_sources(&cfg, &args.selector)?;
    if entries.is_empty() {
        return Ok(CmdOutput {
            stdout: "no sources to index".to_string(),
            ..Default::default()
        });
    }

    let mut outcomes: Vec<IndexOutcome> = Vec::with_capacity(entries.len());
    let mut created: Vec<PathBuf> = Vec::new();
    for entry in &entries {
        if !args.force {
            match check_staleness(entry, deps) {
                StalenessResult::UpToDate(reason) => {
                    outcomes.push(IndexOutcome {
                        name: entry.name.clone(),
                        result: Ok(IndexSuccess {
                            chunks_inserted: 0,
                            head_sha: None,
                            skipped: Some(reason),
                        }),
                    });
                    continue;
                }
                StalenessResult::NeedsRebuild => {}
            }
        }
        match index_one_source(entry, deps, &mut created) {
            Ok(success) => outcomes.push(IndexOutcome {
                name: entry.name.clone(),
                result: Ok(success),
            }),
            Err(e) => {
                tracing::warn!(source = %entry.name, error = %e, "index failed");
                outcomes.push(IndexOutcome {
                    name: entry.name.clone(),
                    result: Err(format!("{e}")),
                });
            }
        }
    }

    // Format output
    let mut warnings = Vec::new();
    let mut lines = Vec::new();
    let mut failures = 0usize;
    for o in &outcomes {
        match &o.result {
            Ok(s) if s.skipped.is_some() => {
                lines.push(format!("skipped '{}': {}", o.name, s.skipped.as_ref().unwrap()));
            }
            Ok(s) => lines.push(format!("indexed '{}': {} chunks", o.name, s.chunks_inserted)),
            Err(msg) => {
                failures += 1;
                let line = format!("failed '{}': {msg}", o.name);
                warnings.push(line.clone());
                lines.push(line);
            }
        }
    }
    let stdout = lines.join("\n");
    if failures == outcomes.len()
        && matches!(args.selector, SourceSelector::Single(_))
        && let Some(IndexOutcome { result: Err(msg), .. }) = outcomes.first()
    {
        return Err(anyhow!(msg.clone()));
    }
    Ok(CmdOutput {
        stdout,
        created_paths: created,
        removed_paths: Vec::new(),
        warnings,
        doctor_failed: None,
    })
}
```

Add staleness check helper:

```rust
enum StalenessResult {
    UpToDate(String),
    NeedsRebuild,
}

fn check_staleness<D: ContextDeps>(entry: &SourceEntry, deps: &D) -> StalenessResult {
    let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);

    let current_head = match source_repo_root(entry) {
        Some(root) if root.exists() => deps.git().head_sha(root).ok().flatten(),
        _ => None,
    };
    let current_model = deps.embedder().model_hash();

    if layout.state.exists() {
        if let Ok(Some(on_disk)) = IndexState::load(&layout.state) {
            let model_matches = on_disk.embedder_model_hash == current_model;
            let head_matches = match (&on_disk.head_sha, &current_head) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            };
            if model_matches && head_matches {
                return StalenessResult::UpToDate(format!(
                    "HEAD {} unchanged",
                    current_head.as_deref().unwrap_or("?")
                ));
            }
        }
    }
    StalenessResult::NeedsRebuild
}
```

Add `skipped` field to `IndexSuccess`:

```rust
struct IndexSuccess {
    chunks_inserted: usize,
    head_sha: Option<String>,
    skipped: Option<String>,
}
```

- [ ] **Step 5: Remove run_refresh and refresh_one_source entirely**

Delete `run_refresh`, `refresh_one_source`, `RefreshOutcome` from `src/context/cli.rs`.

Remove `ContextCmd::Refresh(args) => run_refresh(args, deps)` from the `run_context_cmd` match.

- [ ] **Step 6: Update clap layer**

In `src/cli/mod.rs`:

Remove `ContextRefreshOpts` struct. Add `--force` to `ContextIndexOpts`:

```rust
#[derive(Parser)]
pub struct ContextIndexOpts {
    /// Index a single named source. Mutually exclusive with --all.
    #[arg(long, conflicts_with = "all")]
    pub source: Option<String>,

    /// Index every registered source.
    #[arg(long, conflicts_with = "source")]
    pub all: bool,

    /// Force full rebuild even if sources are unchanged.
    #[arg(long)]
    pub force: bool,
}
```

Remove `Refresh(ContextRefreshOpts)` from `ContextCommand` enum. Add a hidden alias:

```rust
    /// Alias for `index` (deprecated, will be removed).
    #[command(hide = true)]
    Refresh(ContextIndexOpts),
```

- [ ] **Step 7: Update main.rs dispatch**

In `src/main.rs`, change:

```rust
        cli::ContextCommand::Index(i) => ContextCmd::Index(IndexArgs {
            selector: selector(i.source, i.all),
            force: i.force,
        }),
        cli::ContextCommand::Refresh(r) => {
            eprintln!("warning: `refresh` is deprecated, use `index` instead");
            ContextCmd::Index(IndexArgs {
                selector: selector(r.source, r.all),
                force: false,
            })
        }
```

- [ ] **Step 8: Update existing tests**

In `src/context/cli_tests.rs`:

- Update all `IndexArgs { selector: ... }` to include `force: true` (existing tests expect unconditional behavior)
- Change `refresh_skips_when_head_sha_unchanged` to use `IndexArgs { ..., force: false }`
- Change `refresh_rebuilds_on_embedder_model_hash_mismatch` to use `IndexArgs { ..., force: false }`
- Remove all `RefreshArgs` usage — replace with `IndexArgs`
- Remove `RefreshArgs` from the import line

- [ ] **Step 9: Run all tests**

Run: `rtk cargo test --bin quorum -- context::cli_tests -v`
Expected: ALL PASS

- [ ] **Step 10: Commit**

```bash
rtk git add src/cli/mod.rs src/main.rs src/context/cli.rs src/context/cli_tests.rs
rtk git commit -m "refactor(context): merge index and refresh into unified command (#360)"
```

---

### Task 5: Incremental indexing in index_one_source

**Files:**
- Modify: `src/context/cli.rs:991-1059`
- Test: `src/context/cli_tests.rs`

- [ ] **Step 1: Write failing test for incremental behavior**

```rust
#[test]
fn index_incremental_only_reembeds_changed_files() {
    let deps = TestDeps::new();
    seed_single_source(&deps, "mini", "mini-rust");

    // First full index
    let args = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: true,
    };
    run_context_cmd(&ContextCmd::Index(args), &deps).expect("first index");

    let db_path = deps.home_dir().join("sources/mini/index.db");
    let count_before: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0)).unwrap()
    };
    assert!(count_before > 0, "should have chunks after first index");

    // Advance HEAD so staleness check triggers rebuild,
    // but provide an empty diff (no files changed)
    let fixture_root = fixture_path("mini-rust");
    deps.git_mut().set_head(&fixture_root, Some("newheadsha123".to_string()));
    deps.git_mut().set_diff_files(&fixture_root, vec![]);

    let args = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: false,
    };
    let out = run_context_cmd(&ContextCmd::Index(args), &deps).expect("incremental index");

    // With no changed files, chunks should remain the same
    let count_after: i64 = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(count_before, count_after, "no files changed → chunks unchanged");
    assert!(
        out.stdout.contains("indexed 'mini'"),
        "should still report as indexed: {:?}",
        out.stdout
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum -- index_incremental_only_reembeds -v`
Expected: FAIL — current index always does full rebuild

- [ ] **Step 3: Add incremental path to index_one_source**

Modify `index_one_source` in `src/context/cli.rs` to check for an incremental path:

```rust
fn index_one_source<D: ContextDeps>(
    entry: &SourceEntry,
    deps: &D,
    created: &mut Vec<PathBuf>,
) -> Result<IndexSuccess> {
    let layout = SourceLayout::for_source(deps.home_dir(), &entry.name);
    ensure_dir(&layout.dir)?;

    // Check if we can do an incremental update
    let prior_head = layout.state.exists()
        .then(|| IndexState::load(&layout.state).ok().flatten())
        .flatten()
        .and_then(|s| s.head_sha);

    let current_head = match source_repo_root(entry) {
        Some(root) => deps.git().head_sha(root)
            .map_err(|e| anyhow!("git head_sha({}): {e}", root.display()))?,
        None => None,
    };

    // Try incremental: need prior HEAD, current HEAD, git diff, and existing DB
    let incremental = if let (Some(prev), Some(_curr)) = (&prior_head, &current_head) {
        if let Some(root) = source_repo_root(entry) {
            deps.git().diff_files(root, prev)
                .ok()
                .flatten()
                .filter(|_| layout.db.exists())
        } else {
            None
        }
    } else {
        None
    };

    if let Some(changed_files) = incremental {
        if changed_files.is_empty() {
            // HEAD moved but no files changed (e.g. merge commit with no diff)
            // Just update state
            let state = IndexState::new(deps.embedder().model_hash())
                .with_head_sha(current_head.clone())
                .with_indexed_at(deps.clock().now());
            state.save(&layout.state)
                .map_err(|e| anyhow!("save state.json: {e}"))?;
            return Ok(IndexSuccess {
                chunks_inserted: 0,
                head_sha: current_head,
                skipped: None,
            });
        }

        let file_filter: std::collections::HashSet<String> =
            changed_files.into_iter().collect();

        let extracted = extract_source_filtered(
            entry, &ExtractConfig::default(), deps.clock(), Some(&file_filter),
        ).map_err(|e| anyhow!("extract failed for '{}': {e}", entry.name))?;

        let mut builder = IndexBuilder::new(&layout.db, deps.clock(), deps.embedder())
            .map_err(|e| anyhow!("open index db: {e}"))?;
        let report = builder.update_files(
            &entry.name,
            &extracted.chunks,
            &file_filter,
            &extracted.structural_fingerprints,
        ).map_err(|e| anyhow!("incremental update failed for '{}': {e}", entry.name))?;

        let state = IndexState::new(deps.embedder().model_hash())
            .with_head_sha(current_head.clone())
            .with_indexed_at(deps.clock().now());
        state.save(&layout.state)
            .map_err(|e| anyhow!("save state.json: {e}"))?;

        return Ok(IndexSuccess {
            chunks_inserted: report.chunks_inserted,
            head_sha: current_head,
            skipped: None,
        });
    }

    // --- Full rebuild (existing logic) ---
    // ... existing extract_source → ChunkStore → rebuild_from_jsonl → fingerprints → state
}
```

Add `use crate::context::extract::dispatch::extract_source_filtered;` to the imports.

- [ ] **Step 4: Run tests to verify they pass**

Run: `rtk cargo test --bin quorum -- context::cli_tests -v`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
rtk git add src/context/cli.rs
rtk git commit -m "feat(context): incremental indexing via git diff (#361)"
```

---

### Task 6: Progress output during indexing

**Files:**
- Modify: `src/context/cli.rs` (index handler)
- Test: `src/context/cli_tests.rs`

- [ ] **Step 1: Write failing test for progress output**

```rust
#[test]
fn index_output_includes_progress_summary() {
    let deps = TestDeps::new();
    seed_single_source(&deps, "mini", "mini-rust");

    let args = IndexArgs {
        selector: SourceSelector::Single("mini".to_string()),
        force: true,
    };
    let out = run_context_cmd(&ContextCmd::Index(args), &deps).expect("index");

    // Summary line should include file and chunk counts
    assert!(
        out.stdout.contains("files") && out.stdout.contains("chunks"),
        "output should include files/chunks summary: {:?}",
        out.stdout
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `rtk cargo test --bin quorum -- index_output_includes_progress -v`
Expected: FAIL — current output only says "indexed 'mini': N chunks"

- [ ] **Step 3: Enhance output format**

Update the output formatting in `run_index` to include richer summary data. Modify `IndexSuccess` to carry diagnostics:

```rust
struct IndexSuccess {
    chunks_inserted: usize,
    head_sha: Option<String>,
    skipped: Option<String>,
    files_scanned: usize,
    incremental: bool,
    cached_files: usize,
}
```

Update the output formatting:

```rust
Ok(s) if s.skipped.is_some() => {
    lines.push(format!("skipped '{}': {}", o.name, s.skipped.as_ref().unwrap()));
}
Ok(s) if s.incremental => {
    lines.push(format!(
        "indexed '{}': {} chunks ({} files changed, {} cached)",
        o.name, s.chunks_inserted,
        s.files_scanned, s.cached_files
    ));
}
Ok(s) => {
    lines.push(format!(
        "indexed '{}': {} files, {} chunks",
        o.name, s.files_scanned, s.chunks_inserted
    ));
}
```

Populate `files_scanned` from `ExtractResult::diagnostics.extracted_files` in both full and incremental paths.

- [ ] **Step 4: Add stderr progress for TTY sessions**

Add a simple progress reporter that writes counting-up lines to stderr. In `index_one_source`, before the extraction step:

```rust
    let is_tty = std::io::stderr().is_terminal();
    if is_tty {
        eprint!("\x1b[2m  Scanning...\x1b[0m");
    }
```

After extraction:
```rust
    if is_tty {
        eprint!("\r\x1b[K\x1b[2m  Extracting...    {} chunks\x1b[0m", extracted.chunks.len());
    }
```

After embedding (inside builder):
```rust
    if is_tty {
        eprint!("\r\x1b[K\x1b[2m  Storing...       done\x1b[0m\n");
    }
```

Note: For the initial implementation, progress is simple inline eprints. A full progress callback system can be added later if needed — YAGNI.

- [ ] **Step 5: Run all tests**

Run: `rtk cargo test --bin quorum -- context::cli_tests -v`
Expected: ALL PASS

- [ ] **Step 6: Commit**

```bash
rtk git add src/context/cli.rs
rtk git commit -m "feat(context): add progress output during indexing (#342)"
```

---

### Task 7: Update clap tests and final cleanup

**Files:**
- Modify: `src/cli/mod.rs` (test section)
- Modify: `src/context/cli_tests.rs`

- [ ] **Step 1: Update clap mutual-exclusivity tests**

In `src/cli/mod.rs`, update tests that reference `refresh`:

```rust
    #[test]
    fn context_index_rejects_source_and_all() {
        let res = Args::try_parse_from(["quorum", "context", "index", "--source", "foo", "--all"]);
        assert!(res.is_err(), "index --source + --all must conflict");
    }

    #[test]
    fn context_refresh_alias_still_parses() {
        let res = Args::try_parse_from(["quorum", "context", "refresh", "--source", "foo"]);
        assert!(res.is_ok(), "refresh alias must still parse");
    }

    #[test]
    fn context_index_force_flag() {
        let res = Args::try_parse_from(["quorum", "context", "index", "--all", "--force"]);
        assert!(res.is_ok(), "index --all --force must parse");
    }
```

- [ ] **Step 2: Run full test suite**

Run: `rtk cargo test --bin quorum -v`
Expected: ALL PASS

- [ ] **Step 3: Run clippy**

Run: `rtk cargo clippy --all-targets -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Run fmt check**

Run: `rtk cargo fmt -- --check`
Expected: No formatting issues

- [ ] **Step 5: Commit any cleanup**

```bash
rtk git add -A
rtk git commit -m "test: update clap and cli tests for unified index command (#360)"
```

---
