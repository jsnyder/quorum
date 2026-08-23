# AST Symbol-Existence Grounding Check — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans or superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Detect hallucinated LLM findings by verifying that backtick-wrapped symbols in finding titles actually exist at the cited source location, and teach the LLM to emit reasoning + confidence metadata.

**Architecture:** A post-merge, pre-calibrate grounding pass extracts backtick-wrapped identifiers from LLM finding titles, checks them as word-bounded substrings in the source at `[line_start - 2, line_end + 2]`, and sets a `grounding_status` field on each finding. Findings that fail grounding are demoted one severity step. The LLM prompt is updated to request `reasoning` (capped at 200 chars) and `confidence` (0-1 float) fields. Category prompt is NOT changed — the existing `From<String>` shim handles normalization.

**Tech Stack:** Rust 2024, MSRV 1.88, regex crate (already a dependency), serde.

**Design decisions (from 3-reviewer brainstorm):**
- Substring matching for v1 (no tree-sitter) — defer AST-level verification to v2
- Per-language keyword stoplist + minimum identifier length 4 to reduce false-Verified rate
- Use existing `line_start`/`line_end` for grounding — do NOT ask LLM for separate `cited_lines`
- Set `grounding_status` field on Finding, do NOT mutate title text
- Do NOT change prompt categories (rely on `From<String>` mapping)
- Grounding only targets `Source::Llm` findings
- Ablation via `QUORUM_DISABLE_AST_GROUNDING=1`
- Demotion: one severity step down (Critical→High→Medium→Low→Info)
- Scope cuts: no parser caching cleanup, no FpKind::Hallucination auto-set, no category prompt alignment

---

## Task 1: GroundingStatus enum + Finding field

**Files:**
- Modify: `src/finding.rs:49-73` (Finding struct)
- Modify: `src/finding.rs:100-126` (FindingBuilder)
- Test: `tests/finding_schema_test.rs`

**Step 1: Write the failing test**

In `tests/finding_schema_test.rs`, add:

```rust
#[test]
fn grounding_status_serde_roundtrip() {
    use quorum::finding::{FindingBuilder, GroundingStatus};
    for status in [
        GroundingStatus::Verified,
        GroundingStatus::SymbolNotFound,
        GroundingStatus::LineOutOfRange,
        GroundingStatus::NotChecked,
    ] {
        let f = FindingBuilder::new().grounding_status(status.clone()).build();
        let json = serde_json::to_string(&f).unwrap();
        let back: quorum::finding::Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(f.grounding_status, back.grounding_status);
    }
}

#[test]
fn grounding_status_absent_deserializes_as_none() {
    // Old JSON without grounding_status should parse fine
    let json = r#"{"title":"T","description":"D","severity":"info","category":"security","source":"local-ast","line_start":1,"line_end":1,"evidence":[],"similar_precedent":[]}"#;
    let f: quorum::finding::Finding = serde_json::from_str(json).unwrap();
    assert!(f.grounding_status.is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --test finding_schema_test grounding_status -v`
Expected: FAIL — `GroundingStatus` not found

**Step 3: Write minimal implementation**

In `src/finding.rs`, add the enum before the Finding struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroundingStatus {
    Verified,
    SymbolNotFound,
    LineOutOfRange,
    NotChecked,
}
```

Add field to Finding struct (after `cited_lines`):

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounding_status: Option<GroundingStatus>,
```

Add to FindingBuilder `new()` defaults: `grounding_status: None,`

Add builder method:

```rust
    pub fn grounding_status(mut self, s: GroundingStatus) -> Self {
        self.inner.grounding_status = Some(s);
        self
    }
```

Update every Finding literal in the codebase that constructs a Finding directly (not via builder) to include `grounding_status: None`. Key locations:
- `src/review.rs:62-79` (LlmFinding::into_finding)
- `src/analysis.rs` (all Finding literals — search for `Finding {`)
- `src/ast_grep.rs` (Finding literal)
- `src/linter.rs` (Finding literals)

**Step 4: Run tests to verify pass**

Run: `cargo test --test finding_schema_test grounding_status -v`
Expected: PASS

Run: `cargo test --bin quorum && cargo test --lib`
Expected: All pass (1785+)

**Step 5: Commit**

```bash
git add src/finding.rs src/review.rs src/analysis.rs src/ast_grep.rs src/linter.rs tests/finding_schema_test.rs
git commit -m "feat(grounding): add GroundingStatus enum + Finding field"
```

---

## Task 2: Identifier extraction from finding titles

**Files:**
- Create: `src/grounding.rs`
- Modify: `src/lib.rs` (add `pub mod grounding;`)
- Test: inline `#[cfg(test)]` in `src/grounding.rs`

**Step 1: Write the failing tests**

Create `src/grounding.rs` with test-only content:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_backtick_identifiers_from_title() {
        let ids = extract_identifiers("Function `parse_unified_diff` panics on single-line hunks");
        assert_eq!(ids, vec!["parse_unified_diff"]);
    }

    #[test]
    fn extracts_multiple_identifiers() {
        let ids = extract_identifiers("`foo` and `bar_baz` are both wrong");
        assert_eq!(ids, vec!["foo", "bar_baz"]);
    }

    #[test]
    fn returns_empty_for_no_backticks() {
        let ids = extract_identifiers("Missing null check on return value");
        assert!(ids.is_empty());
    }

    #[test]
    fn filters_short_identifiers() {
        // Identifiers < 4 chars are filtered (too common: fn, Ok, let, mut, etc.)
        let ids = extract_identifiers("`fn` and `Ok` and `parse_diff` are mentioned");
        assert_eq!(ids, vec!["parse_diff"]);
    }

    #[test]
    fn filters_language_stopwords() {
        let ids = extract_identifiers("`self` calls `unwrap` on `parse_config`");
        // "self" and "unwrap" are in the Rust stoplist
        assert_eq!(ids, vec!["parse_config"]);
    }

    #[test]
    fn extracts_from_description_too() {
        let ids = extract_identifiers_from_finding_text(
            "Missing error handling",
            "The function `process_data` at line 42 swallows the error",
        );
        assert_eq!(ids, vec!["process_data"]);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib grounding -v`
Expected: FAIL — module `grounding` not found / functions not defined

**Step 3: Write minimal implementation**

Add `pub mod grounding;` to `src/lib.rs` (after `pub mod prompt_sanitize;`).

In `src/grounding.rs`:

```rust
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

static BACKTICK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"`([^`]+)`").unwrap());

static STOPWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        // Rust keywords / ultra-common identifiers
        "self", "Self", "super", "crate", "true", "false", "None", "Some",
        "unwrap", "expect", "clone", "iter", "into", "from", "new", "default",
        "push", "len", "is_empty", "map", "filter", "collect", "Ok", "Err",
        "Result", "Option", "String", "Vec", "Box", "Arc", "Mutex",
        // Python common
        "self", "None", "True", "False", "print", "len", "list", "dict",
        "str", "int", "float", "bool", "type", "super", "init",
        // TypeScript/JS common
        "this", "null", "undefined", "true", "false", "console", "log",
        "push", "map", "filter", "length", "toString", "Promise",
    ]
    .into_iter()
    .collect()
});

const MIN_IDENTIFIER_LEN: usize = 4;

pub fn extract_identifiers(text: &str) -> Vec<&str> {
    BACKTICK_RE
        .captures_iter(text)
        .filter_map(|cap| {
            let id = cap.get(1).unwrap().as_str();
            if id.len() >= MIN_IDENTIFIER_LEN && !STOPWORDS.contains(id) {
                Some(id)
            } else {
                None
            }
        })
        .collect()
}

pub fn extract_identifiers_from_finding_text<'a>(title: &'a str, description: &'a str) -> Vec<&'a str> {
    let mut ids = extract_identifiers(title);
    if ids.is_empty() {
        ids = extract_identifiers(description);
    }
    ids
}
```

**Step 4: Run tests to verify pass**

Run: `cargo test --lib grounding -v`
Expected: PASS (6 tests)

**Step 5: Commit**

```bash
git add src/grounding.rs src/lib.rs
git commit -m "feat(grounding): identifier extraction from finding text"
```

---

## Task 3: Core grounding verification logic

**Files:**
- Modify: `src/grounding.rs`
- Test: inline `#[cfg(test)]` in `src/grounding.rs`

**Step 1: Write the failing tests**

Add to `src/grounding.rs` tests:

```rust
    use crate::finding::{FindingBuilder, GroundingStatus, Severity, Source};

    fn sample_source() -> &'static str {
        // 10 lines of Rust code
        "use std::io;\n\
         \n\
         fn parse_unified_diff(input: &str) -> Vec<Hunk> {\n\
         \    let lines = input.lines();\n\
         \    let mut hunks = Vec::new();\n\
         \    for line in lines {\n\
         \        hunks.push(parse_hunk(line));\n\
         \    }\n\
         \    hunks\n\
         }\n"
    }

    #[test]
    fn grounding_verified_when_symbol_found_at_cited_lines() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` panics on single-line hunks")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::Verified);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_symbol_not_found_when_identifier_absent() {
        let f = FindingBuilder::new()
            .title("Function `nonexistent_func` has a bug")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_line_out_of_range() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` panics")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(50, 60)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::LineOutOfRange);
        assert_eq!(result.severity_change, Some(Severity::Medium));
    }

    #[test]
    fn grounding_not_checked_for_non_llm_source() {
        let f = FindingBuilder::new()
            .title("Function `parse_unified_diff` issue")
            .source(Source::LocalAst)
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::NotChecked);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_not_checked_when_no_identifiers_in_title() {
        let f = FindingBuilder::new()
            .title("Missing null check on return value")
            .description("Some generic description")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::High)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::NotChecked);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_demotion_steps_down_one_level() {
        for (input, expected) in [
            (Severity::Critical, Severity::High),
            (Severity::High, Severity::Medium),
            (Severity::Medium, Severity::Low),
            (Severity::Low, Severity::Info),
        ] {
            let f = FindingBuilder::new()
                .title("Function `nonexistent_func` has a bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(3, 9)
                .severity(input)
                .build();
            let result = verify_grounding(&f, sample_source());
            assert_eq!(result.severity_change, Some(expected));
        }
    }

    #[test]
    fn grounding_info_cannot_demote_further() {
        let f = FindingBuilder::new()
            .title("Function `nonexistent_func` has a bug")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(3, 9)
            .severity(Severity::Info)
            .build();
        let result = verify_grounding(&f, sample_source());
        assert_eq!(result.status, GroundingStatus::SymbolNotFound);
        assert_eq!(result.severity_change, None);
    }

    #[test]
    fn grounding_utf8_multibyte_does_not_panic() {
        let source = "fn process() {\n    let emoji = \"🎉\";\n    let cjk = \"中文\";\n}\n";
        let f = FindingBuilder::new()
            .title("Function `process` has an issue")
            .source(Source::Llm("gpt-5.4".into()))
            .lines(1, 3)
            .severity(Severity::Medium)
            .build();
        let result = verify_grounding(&f, source);
        assert_eq!(result.status, GroundingStatus::Verified);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib grounding::tests::grounding -v`
Expected: FAIL — `verify_grounding` not found

**Step 3: Write minimal implementation**

Add to `src/grounding.rs`:

```rust
use crate::finding::{Finding, GroundingStatus, Severity, Source};

pub struct GroundingResult {
    pub status: GroundingStatus,
    pub severity_change: Option<Severity>,
}

fn demote_severity(s: &Severity) -> Option<Severity> {
    match s {
        Severity::Critical => Some(Severity::High),
        Severity::High => Some(Severity::Medium),
        Severity::Medium => Some(Severity::Low),
        Severity::Low => Some(Severity::Info),
        Severity::Info => None,
    }
}

pub fn verify_grounding(finding: &Finding, source: &str) -> GroundingResult {
    if !matches!(finding.source, Source::Llm(_)) {
        return GroundingResult { status: GroundingStatus::NotChecked, severity_change: None };
    }

    let line_count = source.lines().count() as u32;
    if finding.line_start > line_count || finding.line_end > line_count {
        return GroundingResult {
            status: GroundingStatus::LineOutOfRange,
            severity_change: demote_severity(&finding.severity),
        };
    }

    let identifiers = extract_identifiers_from_finding_text(&finding.title, &finding.description);
    if identifiers.is_empty() {
        return GroundingResult { status: GroundingStatus::NotChecked, severity_change: None };
    }

    let start = (finding.line_start as usize).saturating_sub(3).max(0);
    let end = (finding.line_end as usize + 2).min(line_count as usize);
    let window: String = source
        .lines()
        .skip(start)
        .take(end - start)
        .collect::<Vec<_>>()
        .join("\n");

    let all_found = identifiers.iter().all(|id| {
        let pattern = format!(r"\b{}\b", regex::escape(id));
        Regex::new(&pattern).map_or(false, |re| re.is_search(&window))
    });

    if all_found {
        GroundingResult { status: GroundingStatus::Verified, severity_change: None }
    } else {
        GroundingResult {
            status: GroundingStatus::SymbolNotFound,
            severity_change: demote_severity(&finding.severity),
        }
    }
}
```

Note: The `Regex::new` per-identifier is fine for v1 since there are typically 1-2 identifiers per finding. The `is_search` method may need adjustment based on the regex crate version — use `is_match` if `is_search` is not available.

**Step 4: Run tests to verify pass**

Run: `cargo test --lib grounding -v`
Expected: PASS (14 tests)

**Step 5: Commit**

```bash
git add src/grounding.rs
git commit -m "feat(grounding): core verification logic with demotion"
```

---

## Task 4: Pipeline wiring + ablation

**Files:**
- Modify: `src/pipeline.rs:623-670` (between merge and calibrate)
- Test: `src/pipeline.rs` inline tests or `tests/` integration test

**Step 1: Write the failing test**

Add to `src/grounding.rs` tests:

```rust
    #[test]
    fn apply_grounding_pass_sets_status_and_demotes() {
        let source = "fn parse_unified_diff() {}\nfn other() {}\n";
        let findings = vec![
            FindingBuilder::new()
                .title("Function `parse_unified_diff` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
            FindingBuilder::new()
                .title("AST finding")
                .source(Source::LocalAst)
                .lines(1, 1)
                .severity(Severity::Medium)
                .build(),
        ];
        let result = apply_grounding(findings, source, false);
        assert_eq!(result[0].grounding_status, Some(GroundingStatus::Verified));
        assert_eq!(result[0].severity, Severity::High);
        assert_eq!(result[1].grounding_status, Some(GroundingStatus::SymbolNotFound));
        assert_eq!(result[1].severity, Severity::Medium);
        assert!(result[2].grounding_status.is_none());
        assert_eq!(result[2].severity, Severity::Medium);
    }

    #[test]
    fn apply_grounding_disabled_returns_unchanged() {
        let source = "fn foo() {}\n";
        let findings = vec![
            FindingBuilder::new()
                .title("Function `nonexistent` has bug")
                .source(Source::Llm("gpt-5.4".into()))
                .lines(1, 1)
                .severity(Severity::High)
                .build(),
        ];
        let result = apply_grounding(findings, source, true);
        assert!(result[0].grounding_status.is_none());
        assert_eq!(result[0].severity, Severity::High);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib grounding::tests::apply_grounding -v`
Expected: FAIL — `apply_grounding` not found

**Step 3: Write minimal implementation**

Add to `src/grounding.rs`:

```rust
pub fn apply_grounding(mut findings: Vec<Finding>, source: &str, disabled: bool) -> Vec<Finding> {
    if disabled {
        return findings;
    }
    for finding in &mut findings {
        if !matches!(finding.source, Source::Llm(_)) {
            continue;
        }
        let result = verify_grounding(finding, source);
        finding.grounding_status = Some(result.status);
        if let Some(new_severity) = result.severity_change {
            finding.severity = new_severity;
        }
    }
    findings
}
```

Then wire into `src/pipeline.rs` between merge and calibrate. After line 630 (`let merged = { ... result };`), before the calibrate block:

```rust
    // Grounding: verify LLM findings cite real symbols
    let grounding_disabled = std::env::var("QUORUM_DISABLE_AST_GROUNDING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let merged = crate::grounding::apply_grounding(merged, source, grounding_disabled);
```

Also add `pub use quorum::grounding;` to `src/main.rs` if needed for bin-crate access.

**Step 4: Run tests to verify pass**

Run: `cargo test --lib grounding -v`
Expected: PASS

Run: `cargo test --bin quorum && cargo test --lib`
Expected: All pass

**Step 5: Commit**

```bash
git add src/grounding.rs src/pipeline.rs src/main.rs
git commit -m "feat(grounding): pipeline wiring + QUORUM_DISABLE_AST_GROUNDING ablation"
```

---

## Task 5: LLM prompt update — reasoning + confidence fields

**Files:**
- Modify: `src/llm_client.rs:1083-1096` (response_format section of system prompt)
- Modify: `src/review.rs:32-42` (LlmFinding struct)
- Modify: `src/review.rs:44-80` (into_finding method)
- Test: `src/review.rs` inline tests

**Step 1: Write the failing tests**

Add to `src/review.rs` tests (at the bottom of the test module):

```rust
    #[test]
    fn llm_finding_with_reasoning_and_confidence_parses() {
        let json = r#"[{"title":"Bug","description":"D","severity":"high","category":"security","line_start":1,"line_end":1,"reasoning":"The function lacks bounds checking","confidence":0.85}]"#;
        let findings = parse_llm_response(json, "gpt-5.4").unwrap();
        assert_eq!(findings[0].reasoning.as_deref(), Some("The function lacks bounds checking"));
        assert_eq!(findings[0].confidence, Some(0.85));
    }

    #[test]
    fn llm_finding_without_new_fields_still_parses() {
        let json = r#"[{"title":"Bug","description":"D","severity":"high","category":"security","line_start":1,"line_end":1}]"#;
        let findings = parse_llm_response(json, "gpt-5.4").unwrap();
        assert!(findings[0].reasoning.is_none());
        assert!(findings[0].confidence.is_none());
    }

    #[test]
    fn llm_finding_confidence_clamped_to_0_1() {
        let json = r#"[{"title":"Bug","description":"D","severity":"high","category":"security","line_start":1,"line_end":1,"confidence":1.5}]"#;
        let findings = parse_llm_response(json, "gpt-5.4").unwrap();
        assert_eq!(findings[0].confidence, Some(1.0));
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib review::tests::llm_finding_with_reasoning -v`
Expected: FAIL — field `reasoning` not recognized

**Step 3: Write minimal implementation**

Update `LlmFinding` in `src/review.rs:32-42`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LlmFinding {
    pub title: String,
    pub description: String,
    pub severity: String,
    pub category: String,
    pub line_start: u32,
    pub line_end: u32,
    #[serde(default)]
    pub suggested_fix: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub confidence: Option<f32>,
}
```

Update `into_finding` to wire the new fields:

```rust
        Finding {
            // ... existing fields ...
            reasoning: self.reasoning,
            confidence: self.confidence.map(|c| c.clamp(0.0, 1.0)),
            cited_lines: None,
            grounding_status: None,
        }
```

Update the system prompt `<response_format>` section in `src/llm_client.rs` (around line 1083). Add after the `suggested_fix` line:

```
- reasoning (string, <=200 chars): one-sentence chain-of-thought explaining WHY this is a real defect and what conditions trigger it. Not a restatement of the title.
- confidence (number, 0.0-1.0): your confidence this is a genuine defect vs false positive. 1.0 = certain, 0.5 = unsure, <0.3 = speculative.
```

**Step 4: Run tests to verify pass**

Run: `cargo test --lib review::tests -v`
Expected: PASS (all review tests)

Run: `cargo test --bin quorum && cargo test --lib`
Expected: All pass

**Step 5: Commit**

```bash
git add src/review.rs src/llm_client.rs
git commit -m "feat(grounding): LLM prompt requests reasoning + confidence fields"
```

---

## Task 6: Output rendering of grounding status

**Files:**
- Modify: `src/output/mod.rs` (human-readable output)
- Test: `src/output/mod.rs` inline tests

**Step 1: Write the failing test**

Add to output tests:

```rust
    #[test]
    fn grounding_status_shown_in_human_output() {
        let f = FindingBuilder::new()
            .title("Function `nonexistent` has bug")
            .severity(Severity::Medium)
            .grounding_status(GroundingStatus::SymbolNotFound)
            .build();
        let output = format_finding_human(&f, "src/main.rs");
        assert!(output.contains("[ungrounded]") || output.contains("symbol-not-found"));
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test --bin quorum output::tests::grounding -v`
Expected: FAIL

**Step 3: Write minimal implementation**

In the human output formatter, after the severity/category line, add a grounding annotation when status is SymbolNotFound or LineOutOfRange. The exact location depends on the current output format — find the `format_finding_human` function (or equivalent) and add a conditional line.

**Step 4: Run tests to verify pass**

Run: `cargo test --bin quorum output -v`
Expected: PASS

Run: `cargo test --bin quorum && cargo test --lib`
Expected: All pass

**Step 5: Commit**

```bash
git add src/output/mod.rs
git commit -m "feat(grounding): render grounding status in human output"
```

---

## Task 7: Telemetry counters

**Files:**
- Modify: `src/pipeline.rs` (count grounding outcomes)
- Test: verify counters exist in tracing output

**Step 1: Add tracing span after grounding pass**

In `src/pipeline.rs`, after the `apply_grounding` call, add:

```rust
    {
        let (verified, not_found, out_of_range, not_checked) = merged.iter().fold(
            (0u32, 0u32, 0u32, 0u32),
            |(v, nf, oor, nc), f| match &f.grounding_status {
                Some(crate::finding::GroundingStatus::Verified) => (v + 1, nf, oor, nc),
                Some(crate::finding::GroundingStatus::SymbolNotFound) => (v, nf + 1, oor, nc),
                Some(crate::finding::GroundingStatus::LineOutOfRange) => (v, nf, oor + 1, nc),
                Some(crate::finding::GroundingStatus::NotChecked) => (v, nf, oor, nc + 1),
                None => (v, nf, oor, nc),
            },
        );
        if verified + not_found + out_of_range > 0 {
            tracing::info!(
                phase = "grounding",
                verified, symbol_not_found = not_found, line_out_of_range = out_of_range, not_checked,
                "grounding pass complete"
            );
        }
    }
```

**Step 2: Verify**

Run: `cargo test --bin quorum && cargo test --lib`
Expected: All pass

**Step 3: Commit**

```bash
git add src/pipeline.rs
git commit -m "feat(grounding): tracing telemetry for grounding outcomes"
```

---

## Verification Gates

After all tasks complete:

```bash
cargo test --bin quorum          # ~1800 tests
cargo test --lib                 # ~600 tests  
cargo test --test finding_schema_test  # schema tests
cargo clippy                     # lint clean
cargo build --release            # release build
```

---

## Success Criteria

- All tests pass including new grounding tests
- `cargo clippy` clean
- LLM findings with backtick identifiers get grounding checked
- Findings failing grounding are demoted one severity step
- `GroundingStatus` field set on all LLM findings
- `QUORUM_DISABLE_AST_GROUNDING=1` skips the pass entirely
- Backward-compatible: old Finding JSON without `grounding_status` still parses
- LLM prompt requests reasoning + confidence (backward-compatible — old responses without these fields still parse)
