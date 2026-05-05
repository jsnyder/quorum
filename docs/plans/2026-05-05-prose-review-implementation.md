# Prose Review Modes — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add `--mode plan|docs` to `quorum review` so non-code artifacts get mode-specific LLM prompts while skipping code-only pipeline stages.

**Architecture:** New `ReviewMode` enum threads through CLI → PipelineConfig → LlmReviewer. Pipeline stages gate on mode. System prompt dispatch selects code/plan/docs prompt. Prose content wrapped in `<document>` instead of `<untrusted_code>`.

**Tech Stack:** Rust, clap (CLI), serde (JSON), tokio (async). All changes in existing crate — no new dependencies.

**Design doc:** `docs/plans/2026-05-05-prose-review-modes.md`

---

### Task 1: ReviewMode enum + CLI flag

**Files:**
- Create: `src/review_mode.rs`
- Modify: `src/lib.rs:92` (add module declaration)
- Modify: `src/cli/mod.rs:446` (add --mode flag to ReviewOpts)
- Modify: `src/main.rs:44` (add module declaration)

**Step 1: Create `src/review_mode.rs` with enum + tests**

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewMode {
    #[default]
    Code,
    Plan,
    Docs,
}

impl ReviewMode {
    pub fn is_prose(self) -> bool {
        !matches!(self, ReviewMode::Code)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReviewMode::Code => "code",
            ReviewMode::Plan => "plan",
            ReviewMode::Docs => "docs",
        }
    }
}

impl std::fmt::Display for ReviewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ReviewMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "code" => Ok(ReviewMode::Code),
            "plan" => Ok(ReviewMode::Plan),
            "docs" => Ok(ReviewMode::Docs),
            other => Err(format!("unknown review mode '{}'; expected code, plan, or docs", other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_code() {
        assert_eq!(ReviewMode::default(), ReviewMode::Code);
    }

    #[test]
    fn is_prose_returns_false_for_code() {
        assert!(!ReviewMode::Code.is_prose());
    }

    #[test]
    fn is_prose_returns_true_for_plan_and_docs() {
        assert!(ReviewMode::Plan.is_prose());
        assert!(ReviewMode::Docs.is_prose());
    }

    #[test]
    fn roundtrip_from_str() {
        assert_eq!("plan".parse::<ReviewMode>().unwrap(), ReviewMode::Plan);
        assert_eq!("docs".parse::<ReviewMode>().unwrap(), ReviewMode::Docs);
        assert_eq!("code".parse::<ReviewMode>().unwrap(), ReviewMode::Code);
        assert_eq!("PLAN".parse::<ReviewMode>().unwrap(), ReviewMode::Plan);
    }

    #[test]
    fn unknown_mode_errors() {
        assert!("spec".parse::<ReviewMode>().is_err());
    }

    #[test]
    fn as_str_roundtrip() {
        for mode in [ReviewMode::Code, ReviewMode::Plan, ReviewMode::Docs] {
            assert_eq!(mode.as_str().parse::<ReviewMode>().unwrap(), mode);
        }
    }

    #[test]
    fn serde_roundtrip() {
        let json = serde_json::to_string(&ReviewMode::Plan).unwrap();
        assert_eq!(json, "\"plan\"");
        let back: ReviewMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ReviewMode::Plan);
    }
}
```

**Step 2: Add module declaration to `src/lib.rs`**

After line 92 (`pub mod calibrate;`), add:
```rust
pub mod review_mode;
```

**Step 3: Add `--mode` flag to `src/cli/mod.rs`**

After the `pub caller: Option<String>` field (line 454), add:
```rust
    /// Review mode: code (default), plan, docs
    #[arg(long, default_value = "code")]
    pub mode: crate::review_mode::ReviewMode,
```

Note: clap supports `FromStr` for custom types automatically.

**Step 4: Add `mod review_mode;` to `src/main.rs`**

After line 44 (`mod pipeline;`), add — actually, since `ReviewMode` is in the lib crate, main.rs accesses it via `quorum::review_mode::ReviewMode`. No additional `mod` needed in main.rs.

**Step 5: Run tests**

Run: `cargo test --bin quorum review_mode -v`
Expected: All 7 tests pass.

**Step 6: Commit**

```
feat(cli): add ReviewMode enum and --mode flag
```

---

### Task 2: Thread mode through PipelineConfig

**Files:**
- Modify: `src/pipeline.rs:121-195` (add mode to PipelineConfig + Default)
- Modify: `src/main.rs:523+` (extract mode from opts, pass to config)

**Step 1: Write test for PipelineConfig mode field**

In `src/pipeline.rs` tests section, add:
```rust
#[test]
fn pipeline_config_default_mode_is_code() {
    let cfg = PipelineConfig::default();
    assert_eq!(cfg.mode, quorum::review_mode::ReviewMode::Code);
}
```

**Step 2: Run test — expect FAIL (no `mode` field)**

**Step 3: Add mode field to PipelineConfig**

In `src/pipeline.rs`, add to `PipelineConfig` struct after `pub focus: Option<String>,` (line 166):
```rust
    /// Review mode: code (default), plan, docs. Controls which pipeline
    /// stages run and which system prompt is sent to the LLM.
    pub mode: quorum::review_mode::ReviewMode,
```

And in the `Default` impl after `focus: None,` (line 192):
```rust
            mode: quorum::review_mode::ReviewMode::Code,
```

**Step 4: Wire mode from CLI opts to PipelineConfig in `src/main.rs`**

Find where `PipelineConfig` is constructed in `run_review()` (around line 620-650) and add:
```rust
            mode: opts.mode,
```

**Step 5: Run test — expect PASS**

Run: `cargo test --bin quorum pipeline_config_default_mode -v`

**Step 6: Run full test suite to check no regressions**

Run: `cargo test --bin quorum`

**Step 7: Commit**

```
feat(pipeline): thread ReviewMode through PipelineConfig
```

---

### Task 3: Gate pipeline stages on mode

**Files:**
- Modify: `src/pipeline.rs:395-532` (skip AST, ast-grep, hydration, Context7, grounding for prose)

**Step 1: Write tests for stage skipping**

In `src/pipeline.rs` tests, add tests using a mock reviewer that verify:
- prose mode skips local AST (0 LocalAst findings)
- prose mode skips ast-grep (0 AstGrep findings)
- prose mode still calls LLM
- code mode still runs all stages

These tests should use the existing `MockReviewer` pattern in the test section.

**Step 2: Implement stage gating**

In `review_file()`, wrap each code-only stage:

At line ~395 (local AST):
```rust
let local_findings = if !pipeline_config.mode.is_prose() {
    // existing AST analysis code...
} else {
    vec![]
};
```

At line ~416 (ast-grep):
```rust
if !pipeline_config.mode.is_prose() {
    // existing ast-grep code...
}
```

At line ~474 (hydration): skip for prose mode — prose files don't have function signatures.

At line ~532 (Context7): skip for prose mode unless explicitly enabled. The existing `context7_skip_reason()` function should be extended:
```rust
fn context7_skip_reason(cfg: &PipelineConfig) -> Option<&'static str> {
    if cfg.mode.is_prose() {
        return Some("prose review mode (use --context7 to enable)");
    }
    // existing checks...
}
```

At line ~722 (grounding): skip for prose mode — no AST symbols to verify.

**Step 3: Run tests — expect PASS**

Run: `cargo test --bin quorum`

**Step 4: Commit**

```
feat(pipeline): gate AST/linter/hydration/grounding stages on ReviewMode
```

---

### Task 4: Mode-aware system prompt dispatch

**Files:**
- Modify: `src/main.rs` (add `mod prose_prompts;` — actually it's in lib, add `pub mod prose_prompts;` to `src/lib.rs`)
- Modify: `src/lib.rs` (add prose_prompts module)
- Modify: `src/llm_client.rs:869-870,936-937` (accept system prompt as parameter)
- Modify: `src/pipeline.rs:101-102` (update LlmReviewer trait to accept system prompt)

**Step 1: Register prose_prompts module in `src/lib.rs`**

After `pub mod review_mode;`:
```rust
pub mod prose_prompts;
```

**Step 2: Update `LlmReviewer` trait to accept system prompt**

In `src/pipeline.rs:101-102`, change:
```rust
pub trait LlmReviewer: Send + Sync {
    fn review(
        &self,
        prompt: &str,
        model: &str,
        system_prompt: &str,
    ) -> anyhow::Result<crate::llm_client::LlmResponse>;
}
```

**Step 3: Update `OpenAiClient` implementation**

In `src/llm_client.rs`, update `chat_completion` and `responses_api` to accept system_prompt parameter instead of calling `Self::system_prompt()`:

`chat_completion(model, prompt, system_prompt)` — use `system_prompt` at line 870 instead of `Self::system_prompt()`.

`responses_api(model, prompt, system_prompt)` — use `system_prompt` at line 937 instead of `Self::system_prompt()`.

Update the `review()` method on the `LlmReviewer` impl to accept and forward `system_prompt`.

**Step 4: Update pipeline call sites**

In `src/pipeline.rs`, where `reviewer.review(&prompt, model)` is called (lines 664, 1069), select the system prompt based on mode:

```rust
let sys_prompt = match pipeline_config.mode {
    quorum::review_mode::ReviewMode::Plan => quorum::prose_prompts::plan_system_prompt(),
    quorum::review_mode::ReviewMode::Docs => quorum::prose_prompts::docs_system_prompt(),
    quorum::review_mode::ReviewMode::Code => crate::llm_client::OpenAiClient::system_prompt(),
};
// ...
reviewer.review(&prompt, model, sys_prompt)
```

**Step 5: Update all MockReviewer implementations in tests**

Every test MockReviewer needs the new signature. Add `_system_prompt: &str` parameter.

**Step 6: Run full test suite**

Run: `cargo test --bin quorum`

**Step 7: Commit**

```
feat(llm): dispatch mode-specific system prompt (plan/docs/code)
```

---

### Task 5: Prose-mode prompt building (document tag)

**Files:**
- Modify: `src/review.rs:8-50` (add mode to ReviewRequest)
- Modify: `src/review.rs:162-280` (build_review_prompt adapts for prose)
- Modify: `src/pipeline.rs:630-652` (pass mode to ReviewRequest)

**Step 1: Write test for prose prompt format**

In `src/review.rs` tests, add:
```rust
#[test]
fn build_prompt_uses_document_tag_in_prose_mode() {
    let req = ReviewRequest {
        file_path: "docs/plan.md".into(),
        language: "markdown".into(),
        code: "# My Plan\n\nPhase 1: do stuff".into(),
        mode: quorum::review_mode::ReviewMode::Plan,
        // ... other fields None/default
    };
    let prompt = build_review_prompt(&req);
    assert!(prompt.contains("<document>"));
    assert!(!prompt.contains("<untrusted_code>"));
    assert!(!prompt.contains("<file_metadata>"));
}
```

**Step 2: Add `mode` field to `ReviewRequest`**

In `src/review.rs`, add to the struct:
```rust
pub mode: quorum::review_mode::ReviewMode,
```

**Step 3: Update `build_review_prompt()` for prose mode**

When `req.mode.is_prose()`:
- Skip `<file_metadata>` block (no language/path metadata needed)
- Wrap content in `<document>` instead of `<untrusted_code>` with code fence
- Skip language-specific fence formatting
- Keep `<historical_findings>`, `<focus_areas>`, and `<referenced_context>` blocks

```rust
if req.mode.is_prose() {
    prompt.push_str("<document>\n");
    prompt.push_str(&defang_sandbox_tags(&req.code));
    prompt.push_str("\n</document>\n");
} else {
    // existing <file_metadata> + <untrusted_code> blocks
}
```

**Step 4: Wire mode through pipeline → ReviewRequest**

In `src/pipeline.rs:630-652`, add `mode: pipeline_config.mode,` to the `ReviewRequest` construction.

**Step 5: Update all existing ReviewRequest constructions in tests**

Add `mode: quorum::review_mode::ReviewMode::Code` to existing test ReviewRequest instances.

**Step 6: Run tests — expect PASS**

Run: `cargo test --bin quorum`

**Step 7: Commit**

```
feat(review): wrap prose content in <document> tag, skip file_metadata
```

---

### Task 6: ReviewRecord mode field + telemetry

**Files:**
- Modify: `src/review_log.rs:233-259` (add mode field to ReviewRecord)
- Modify: `src/main.rs` (set mode on ReviewRecord)

**Step 1: Write test for mode field serialization**

In `src/review_log.rs` tests:
```rust
#[test]
fn review_record_mode_field_serializes() {
    let mut rec = ReviewRecord { /* ... */ };
    rec.mode = Some("plan".into());
    let json = serde_json::to_string(&rec).unwrap();
    assert!(json.contains("\"mode\":\"plan\""));
}

#[test]
fn review_record_mode_defaults_to_none_for_backcompat() {
    let json = r#"{"run_id":"test","timestamp":"2026-05-05T00:00:00Z", ...}"#;
    let rec: ReviewRecord = serde_json::from_str(json).unwrap();
    assert!(rec.mode.is_none());
}
```

**Step 2: Add mode field to ReviewRecord**

In `src/review_log.rs`, after `pub context: ContextTelemetry,` (line 258):
```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
```

**Step 3: Set mode in main.rs when constructing ReviewRecord**

In `run_review()` where the ReviewRecord is built, add:
```rust
mode: if opts.mode == ReviewMode::Code { None } else { Some(opts.mode.as_str().to_string()) },
```

**Step 4: Run tests — expect PASS**

Run: `cargo test --bin quorum`

**Step 5: Commit**

```
feat(telemetry): add mode field to ReviewRecord
```

---

### Task 7: Prose file validation + error hint

**Files:**
- Modify: `src/main.rs` (in run_review, validate file types against mode)

**Step 1: Add validation logic**

In `run_review()`, after extracting opts.mode, add validation:

```rust
let prose_extensions = ["md", "txt", "adoc", "rst"];
if opts.mode == ReviewMode::Code {
    for f in &opts.files {
        if let Some(ext) = f.extension().and_then(|e| e.to_str()) {
            if prose_extensions.contains(&ext.to_lowercase().as_str()) {
                eprintln!(
                    "warning: '{}' looks like a prose file. Use --mode plan or --mode docs for non-code review.",
                    f.display()
                );
            }
        }
    }
}
```

This is a warning, not a hard error, to avoid breaking existing workflows.

**Step 2: Run full test suite**

Run: `cargo test --bin quorum`

**Step 3: Commit**

```
feat(cli): warn when reviewing prose files without --mode
```

---

### Task 8: Integration smoke test

**Files:**
- Modify: `tests/` (add a prose review integration test, or add to existing CLI tests)

**Step 1: Write CLI integration test**

Create a test that runs `quorum review docs/plans/2026-05-05-prose-review-modes.md --mode plan --json` and verifies:
- Exit code is 0 or 1 (not 3/error)
- Output is valid JSON
- No AST/linter findings in output (only LLM findings, if API key available)

This may need to be a `#[ignore]` test if it requires a live LLM endpoint.

**Step 2: Write unit test verifying end-to-end prompt construction**

In `src/review.rs` tests, build a full ReviewRequest in plan mode and verify the prompt:
- Contains `<document>` tag
- Does NOT contain `<untrusted_code>`
- Does NOT contain `<file_metadata>`
- Contains the actual document content

**Step 3: Run full test suite**

Run: `cargo test --bin quorum`

**Step 4: Final commit**

```
test: add prose review mode integration smoke test
```

---

## Task Summary

| Task | Description | Key Files |
|------|------------|-----------|
| 1 | ReviewMode enum + --mode CLI flag | review_mode.rs, cli/mod.rs, lib.rs |
| 2 | Thread mode through PipelineConfig | pipeline.rs, main.rs |
| 3 | Gate pipeline stages on mode | pipeline.rs |
| 4 | Mode-aware system prompt dispatch | llm_client.rs, pipeline.rs, lib.rs |
| 5 | Prose prompt building (document tag) | review.rs, pipeline.rs |
| 6 | ReviewRecord mode field + telemetry | review_log.rs, main.rs |
| 7 | Prose file validation + warning | main.rs |
| 8 | Integration smoke test | tests/, review.rs |
