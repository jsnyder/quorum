# Review Context Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify `review_file` and `review_file_llm_only` in `src/pipeline.rs`, extracting shared context assembly into `build_file_context()` returning an immutable `FileContext` struct.

**Architecture:** Both review functions share ~60% of their logic (feedback index, Context7 enrichment, precedent lookup, prompt building, model loop, merge, grounding, diff classification, calibration). This refactor extracts that shared logic into a `build_file_context()` helper, then merges both functions into a single `review_file` that accepts `Option<AstContext>`. The result is ~250 fewer lines and a single code path for all reviews.

**Tech Stack:** Rust, existing quorum crate types

---

### Task 1: Add AstContext and FileContext structs

**Files:**
- Modify: `src/pipeline.rs:112-205` (after existing type definitions)

- [ ] **Step 1: Write failing test — AstContext constructs from parts**

```rust
#[test]
fn ast_context_can_be_constructed() {
    use crate::ast_grep::RuleMetadata;
    use std::collections::HashMap;

    let source = "fn main() {}";
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    let ctx = AstContext {
        tree: &tree,
        language: crate::parser::Language::Rust,
        rule_metadata: HashMap::new(),
    };
    assert!(ctx.rule_metadata.is_empty());
}
```

Add this test in the `#[cfg(test)] mod tests` block of `src/pipeline.rs`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -- ast_context_can_be_constructed`
Expected: FAIL — `AstContext` not defined.

- [ ] **Step 3: Define AstContext struct**

Add after the `JudgeMetrics` struct (around line 138) in `src/pipeline.rs`:

```rust
/// Pre-parsed AST context for a file. When `Some`, enables local AST
/// analysis, ast-grep rules, judge phase, and hydration context assembly.
/// When `None`, the review runs LLM-only.
pub struct AstContext<'a> {
    pub tree: &'a tree_sitter::Tree,
    pub language: crate::parser::Language,
    pub rule_metadata: std::collections::HashMap<String, crate::ast_grep::RuleMetadata>,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -- ast_context_can_be_constructed`
Expected: PASS

- [ ] **Step 5: Write failing test — FileContext constructs with defaults**

```rust
#[test]
fn file_context_defaults_to_empty() {
    let ctx = FileContext::default();
    assert!(ctx.redacted_code.is_empty());
    assert!(ctx.framework_docs.is_none());
    assert!(ctx.feedback_precedents.is_none());
    assert!(ctx.hydration_context.is_none());
    assert!(ctx.context_block.is_none());
    assert!(ctx.truncation_notice.is_none());
}
```

- [ ] **Step 6: Run test to verify it fails**

Run: `cargo test -- file_context_defaults_to_empty`
Expected: FAIL — `FileContext` not defined.

- [ ] **Step 7: Define FileContext struct**

Add after `AstContext` in `src/pipeline.rs`:

```rust
/// Pre-computed, immutable context for a single file review. Built once
/// by `build_file_context()` and consumed by `ReviewRequest` population.
/// Does NOT contain mutable findings — those stay in the caller's scope.
#[derive(Default)]
pub struct FileContext {
    pub redacted_code: String,
    pub truncation_notice: Option<String>,
    pub framework_docs: Option<Vec<String>>,
    pub feedback_precedents: Option<Vec<String>>,
    pub hydration_context: Option<crate::hydration::HydrationContext>,
    pub context_block: Option<String>,
    pub enrichment_metrics: crate::context_enrichment::EnrichmentMetrics,
    pub context_telemetry: Option<crate::review_log::ContextTelemetry>,
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -- file_context_defaults_to_empty`
Expected: PASS

- [ ] **Step 9: Run full test suite for regressions**

Run: `cargo test --bin quorum`
Expected: All pass — this is purely additive.

- [ ] **Step 10: Commit**

```bash
git add src/pipeline.rs
git commit -m "refactor: add AstContext and FileContext structs (#339)"
```

---

### Task 2: Extract build_file_context()

**Files:**
- Modify: `src/pipeline.rs`

This is the core extraction. `build_file_context()` pulls shared logic from both `review_file` (lines 790-1005) and `review_file_llm_only` (lines 1404-1473) into a single function.

- [ ] **Step 1: Write failing test — build_file_context returns redacted code**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn build_file_context_redacts_secrets() {
    let config = PipelineConfig::default();
    let ctx = build_file_context(
        std::path::Path::new("test.rs"),
        "let api_key = \"sk-secret123\";",
        None, // no AST
        &config,
    )
    .await
    .unwrap();
    assert!(
        !ctx.redacted_code.contains("sk-secret123"),
        "FileContext must contain redacted code, got: {}",
        ctx.redacted_code
    );
    assert!(
        ctx.redacted_code.contains("REDACTED"),
        "redacted code must replace secrets"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -- build_file_context_redacts_secrets`
Expected: FAIL — `build_file_context` not defined.

- [ ] **Step 3: Implement build_file_context()**

Create the function in `src/pipeline.rs`. This extracts the shared logic from `review_file` lines 790-1005 and `review_file_llm_only` lines 1404-1473.

```rust
/// Assemble immutable per-file context for a review. Performs secret
/// redaction, truncation, Context7 enrichment, feedback precedent
/// selection, hydration context assembly (if AST available), and
/// context injection. Returns a `FileContext` that feeds directly
/// into `ReviewRequest` population.
pub(crate) async fn build_file_context(
    file_path: &std::path::Path,
    source: &str,
    ast: Option<&AstContext<'_>>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileContext> {
    let mut ctx = FileContext::default();

    // --- Secret redaction + truncation ---
    // (from review_file lines 808-815)
    let redacted = crate::redact::redact_secrets(source);
    let max_chars = pipeline_config.max_source_chars.unwrap_or(60_000);
    if redacted.len() > max_chars {
        ctx.redacted_code = redacted[..max_chars].to_string();
        ctx.truncation_notice = Some(format!(
            "Source truncated from {} to {} characters.",
            redacted.len(),
            max_chars
        ));
    } else {
        ctx.redacted_code = redacted;
    }

    // --- Feedback precedent selection ---
    // (from review_file lines 951-959)
    let file_str = file_path.to_string_lossy();
    let lang_str = ast
        .map(|a| a.language.as_str())
        .unwrap_or("unknown");

    if let Some(ref shared_index) = pipeline_config.shared_feedback_index {
        let mut index = shared_index.lock().unwrap_or_else(|e| e.into_inner());
        let precedents = query_feedback_precedents(
            &mut index,
            &file_str,
            lang_str,
            &ctx.redacted_code,
        );
        if !precedents.is_empty() {
            ctx.feedback_precedents = Some(precedents);
        }
    }

    // --- Hydration context (AST-only) ---
    // (from review_file lines 790-861)
    if let Some(ast_ctx) = ast {
        {
            let hctx = crate::hydration::build_hydration_context(
                source,
                ast_ctx.tree,
                ast_ctx.language,
            );
            let redacted_ctx = crate::hydration::HydrationContext {
                callee_signatures: hctx
                    .callee_signatures
                    .iter()
                    .map(|s| crate::redact::redact_secrets(s))
                    .collect(),
                type_definitions: hctx
                    .type_definitions
                    .iter()
                    .map(|s| crate::redact::redact_secrets(s))
                    .collect(),
                callers: hctx
                    .callers
                    .iter()
                    .map(|s| crate::redact::redact_secrets(s))
                    .collect(),
                import_targets: hctx.import_targets.clone(),
                qualified_names: hctx.qualified_names.clone(),
            };
            ctx.hydration_context = Some(redacted_ctx);
        }
    }

    // --- Context7 enrichment ---
    // (from review_file lines 863-949, review_file_llm_only 1404-1463)
    if !pipeline_config.context7_disabled {
        let import_targets = ctx
            .hydration_context
            .as_ref()
            .map(|h| h.import_targets.clone())
            .unwrap_or_default();

        if let Some(ref fetcher) = pipeline_config.context7_fetcher {
            let enrichment = crate::context_enrichment::enrich_for_review_in_project(
                file_path,
                &import_targets,
                ast.map(|a| a.language),
                fetcher.as_ref(),
                pipeline_config.live_registry,
                pipeline_config.framework_override.as_deref(),
            )
            .await;
            ctx.framework_docs = if enrichment.docs.is_empty() {
                None
            } else {
                Some(enrichment.docs)
            };
            ctx.enrichment_metrics = enrichment.metrics;
        }
    }

    // --- Context injection ---
    // (from review_file lines 961-1005, skipped in review_file_llm_only)
    if let Some(ref injector) = pipeline_config.context_injector {
        let injection_req = crate::context::inject::InjectionRequest {
            file_path: file_path.to_string_lossy().to_string(),
            code: &ctx.redacted_code,
            import_targets: ctx
                .hydration_context
                .as_ref()
                .map(|h| &h.import_targets[..])
                .unwrap_or(&[]),
        };
        match injector.inject(&injection_req).await {
            Ok(outcome) => {
                ctx.context_block = outcome.rendered_block;
                ctx.context_telemetry = Some(outcome.telemetry);
            }
            Err(e) => {
                tracing::warn!(error = %e, "context injection failed");
            }
        }
    }

    Ok(ctx)
}
```

**Important:** The exact implementation will need to match the actual parameters and methods used in the existing code. The function bodies above are templates — the implementer must read the existing `review_file` lines 790-1005 and `review_file_llm_only` lines 1404-1473 and extract the actual logic, preserving all tracing spans, error handling, and metric accumulation. The structure above shows the correct sequencing and field assignments.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -- build_file_context_redacts_secrets`
Expected: PASS

- [ ] **Step 5: Write test — build_file_context without AST skips hydration**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn build_file_context_without_ast_skips_hydration() {
    let config = PipelineConfig::default();
    let ctx = build_file_context(
        std::path::Path::new("test.rs"),
        "fn main() {}",
        None,
        &config,
    )
    .await
    .unwrap();
    assert!(
        ctx.hydration_context.is_none(),
        "no AST means no hydration context"
    );
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -- build_file_context_without_ast_skips_hydration`
Expected: PASS

- [ ] **Step 7: Write test — truncation notice on oversized source**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn build_file_context_truncates_large_source() {
    let mut config = PipelineConfig::default();
    config.max_source_chars = Some(100);
    let big_source = "x".repeat(200);
    let ctx = build_file_context(
        std::path::Path::new("test.rs"),
        &big_source,
        None,
        &config,
    )
    .await
    .unwrap();
    assert!(ctx.truncation_notice.is_some());
    assert!(
        ctx.redacted_code.len() <= 100,
        "code must be truncated to max_source_chars"
    );
}
```

- [ ] **Step 8: Run test to verify it passes**

Run: `cargo test -- build_file_context_truncates_large_source`
Expected: PASS

- [ ] **Step 9: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All pass — `build_file_context` is additive, nothing calls it yet.

- [ ] **Step 10: Commit**

```bash
git add src/pipeline.rs
git commit -m "refactor: extract build_file_context() for shared context assembly (#339)"
```

---

### Task 3: Refactor review_file to use build_file_context()

**Files:**
- Modify: `src/pipeline.rs:637-1193` (the `review_file` function)

- [ ] **Step 1: Replace inline context assembly with build_file_context() call**

In `review_file`, replace the context assembly blocks (roughly lines 790-1005: secret redaction, truncation, hydration context, Context7 enrichment, feedback precedents, context injection) with a single call:

```rust
// Replace ~215 lines of inline context assembly with:
let ast_ctx = AstContext {
    language: lang,
    rule_metadata: rule_metadata.clone(),
};
let file_ctx = build_file_context(
    file_path,
    source,
    Some(&ast_ctx),
    pipeline_config,
)
.await?;
```

**Prompt ordering constraint:** When populating `ReviewRequest` from `FileContext`, preserve the existing field order in `build_review_prompt` — stable context (system prompt, file context, framework docs, precedents) first, variable content (focus directives, mode) last. This maximizes LLM API prefix cache hits for multi-perspective reviews. Don't reorder sections for cache optimization if it would break logical structure.

Then update the `ReviewRequest` construction (around line 1008-1024) to read from `file_ctx`:

```rust
let req = crate::review::ReviewRequest {
    file_path: file_path.to_string_lossy().to_string(),
    language: lang.as_str().to_string(),
    code: file_ctx.redacted_code.clone(),
    hydration_context: file_ctx.hydration_context.clone(),
    framework_docs: file_ctx.framework_docs.clone(),
    feedback_precedents: file_ctx.feedback_precedents.clone(),
    context_block: file_ctx.context_block.clone(),
    truncation_notice: file_ctx.truncation_notice.clone(),
    focus: pipeline_config.focus.clone(),
    mode: pipeline_config.mode,
};
```

And update the return value to use `file_ctx.enrichment_metrics` and `file_ctx.context_telemetry`.

- [ ] **Step 2: Run full test suite to verify no regressions**

Run: `cargo test --bin quorum`
Expected: All existing tests pass. This is a behavior-preserving refactor — the same logic runs, just from a different call site.

- [ ] **Step 3: Commit**

```bash
git add src/pipeline.rs
git commit -m "refactor: review_file uses build_file_context() (#339)"
```

---

### Task 4: Unify review_file signature to accept Option\<AstContext\>

**Files:**
- Modify: `src/pipeline.rs` (review_file signature + body)

- [ ] **Step 1: Change review_file signature**

Change from:

```rust
pub(crate) async fn review_file(
    file_path: &Path,
    source: &str,
    lang: Language,
    tree: &Tree,
    reviewer: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileReviewResult>
```

To:

```rust
pub(crate) async fn review_file(
    file_path: &Path,
    source: &str,
    ast: Option<AstContext<'_>>,
    reviewer: Option<&dyn LlmReviewer>,
    pipeline_config: &PipelineConfig,
) -> anyhow::Result<FileReviewResult>
```

- [ ] **Step 2: Guard AST-only phases on ast.is_some()**

Wrap the AST-only blocks (local AST analysis, ast-grep rules, judge phase) with:

```rust
let mut all_sources: Vec<Vec<Finding>> = Vec::new();

if let Some(ref ast_ctx) = ast {
    // Local AST complexity analysis (lines 675-694)
    let local_findings = crate::analysis::analyze(source, ast_ctx.language);
    all_sources.push(local_findings);

    // ast-grep rules (lines 696-725)
    let ast_grep_findings = crate::ast_grep::scan_with_rules(
        file_path, source, &ast_ctx.rule_metadata,
    );
    all_sources.push(ast_grep_findings);

    // Judge phase (lines 727-778)
    if pipeline_config.judge_enabled {
        // ... judge logic, operates on all_sources via &mut ...
    }
} else {
    // LLM-only path: no local findings
    all_sources.push(Vec::new());
}
```

- [ ] **Step 3: Derive language string for non-AST path**

For the LLM-only path, derive language from the file extension for the `ReviewRequest.language` field:

```rust
let lang_str = match &ast {
    Some(ref ctx) => ctx.language.as_str().to_string(),
    None => file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown")
        .to_string(),
};
```

Use `lang_str` in the `ReviewRequest` construction.

- [ ] **Step 4: Update review_source to pass AstContext**

In `review_source` (line 1323-1336), construct `AstContext` from the parsed tree and language:

```rust
let ast_ctx = AstContext {
    language: lang,
    rule_metadata: crate::ast_grep::load_rule_metadata(file_path),
};
review_file(
    file_path,
    source,
    Some(ast_ctx),
    reviewer,
    pipeline_config,
)
.await
```

**Note:** Check how `rule_metadata` is currently loaded in `review_file` and replicate that — it may use `load_rules_from_dirs` or similar. The implementer must match the existing loading pattern.

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add src/pipeline.rs
git commit -m "refactor: review_file accepts Option<AstContext> (#339)"
```

---

### Task 5: Delete review_file_llm_only and update call sites

**Files:**
- Modify: `src/pipeline.rs:1362-1625` (delete `review_file_llm_only`)
- Modify: `src/main.rs` (update call sites in `run_review`)

- [ ] **Step 1: Write test — review_file with None AST matches old LLM-only behavior**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn review_file_without_ast_runs_llm_only_path() {
    // Verify that review_file(path, source, None, ...) produces
    // findings equivalent to the old review_file_llm_only path
    let config = PipelineConfig::default();
    let result = review_file(
        std::path::Path::new("test.py"),
        "def foo():\n    eval(input())\n",
        None, // LLM-only
        None, // no reviewer
        &config,
    )
    .await
    .unwrap();
    // Without LLM, LLM-only path produces empty findings
    // (no AST analysis, no LLM review)
    assert!(result.findings.is_empty());
    assert_eq!(result.judge_metrics, JudgeMetrics::default());
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -- review_file_without_ast_runs_llm_only_path`
Expected: PASS — unified `review_file` with `None` AST should work.

- [ ] **Step 3: Update main.rs call sites**

In `src/main.rs`, find the call sites where `review_file_llm_only` is called (around lines 1318-1335 for serial mode, and 1469-1479 for parallel mode). Replace:

```rust
// Old:
if let Some(lang) = Language::from_path(file_path) {
    pipeline::review_source(file_path, &source, lang, llm, &cfg, Some(&cache)).await
} else {
    pipeline::review_file_llm_only(file_path, &source, llm, &cfg).await
}

// New:
if let Some(lang) = Language::from_path(file_path) {
    pipeline::review_source(file_path, &source, lang, llm, &cfg, Some(&cache)).await
} else {
    pipeline::review_file(file_path, &source, None, llm, &cfg).await
}
```

Apply this change to both the serial and parallel code paths.

- [ ] **Step 4: Delete review_file_llm_only**

Remove the entire `review_file_llm_only` function (lines 1362-1625, approximately 263 lines).

Also remove any `pub(crate)` export of `review_file_llm_only` if it exists in `mod.rs` or lib exports.

- [ ] **Step 5: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All pass. If any tests called `review_file_llm_only` directly, update them to call `review_file` with `ast: None`.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy`
Expected: No warnings. Watch for unused imports that were only needed by the deleted function.

- [ ] **Step 7: Run rustfmt**

Run: `cargo fmt -- --check`
Expected: No diffs.

- [ ] **Step 8: Commit**

```bash
git add src/pipeline.rs src/main.rs
git commit -m "refactor: delete review_file_llm_only, unify into review_file (#339)"
```

---

### Task 6: Final verification and cleanup

**Files:**
- Possibly modify: `src/pipeline.rs` (dead code removal)

- [ ] **Step 1: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy`
Expected: No warnings.

- [ ] **Step 3: Run rustfmt**

Run: `cargo fmt -- --check`
Expected: No diffs.

- [ ] **Step 4: Verify line count reduction**

Run: `wc -l src/pipeline.rs`
Expected: ~250 fewer lines than pre-refactor (was ~1625 lines, `review_file_llm_only` was ~263 lines, shared extraction saves additional lines from `review_file`).

- [ ] **Step 5: Run release build**

Run: `cargo build --release`
Expected: Compiles without warnings.

- [ ] **Step 6: Commit any cleanup**

```bash
git add -A
git commit -m "refactor: clean up dead code from review context extraction (#339)"
```
