# TDD Anti-Pattern Guide for 8 New Features

Based on analysis of quorum's existing 492 tests across 31 modules, integration tests in `tests/cli.rs`, and the `FakeReviewer` trait-based test double pattern.

## Existing Patterns (for consistency)

- **Unit tests:** in-file `#[cfg(test)] mod tests` blocks, pure function input/output
- **Integration tests:** `tests/cli.rs` using `assert_cmd` + `predicates`, isolated via `HOME=/tmp/quorum-test-home`
- **Test doubles:** `src/test_support.rs` provides `FakeReviewer` implementing `LlmReviewer` trait
- **File I/O tests:** `tempfile::TempDir` for JSONL read/write (feedback, auto_calibrate)
- **No mocking framework:** no mockall/mockito; fakes are hand-rolled via traits

---

## Feature-by-Feature Guidance

### 1. Token Extraction from LLM Responses

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit only. Parse JSON structs, no HTTP needed. |
| **Mock/Real** | Neither -- test the *parsing function* with literal JSON strings. Do NOT test through the HTTP client. |
| **Key anti-patterns** | **#5 (Testing Implementation):** Do NOT test that `reqwest` was called with specific headers. Test `parse_usage(json_str) -> TokenUsage`. **#4 (Wrong Functionality):** Test edge cases: missing `usage` field, null tokens, `completion_tokens_details`, streaming chunked responses. |
| **Rust pitfall** | Extract a `fn parse_usage(value: &serde_json::Value) -> Option<TokenUsage>` as a pure function. If you bury parsing inside `async fn chat_completion`, you can't unit-test it without an HTTP mock server. The current `llm_client.rs` has zero tests -- this is your opportunity to make it testable. |
| **TDD approach** | Write tests first for: valid response, missing usage key, zero tokens, malformed JSON. The struct is simple enough that TDD adds real value here. |

```rust
// Good: pure function, easy to test
fn parse_usage(body: &serde_json::Value) -> Option<TokenUsage> { ... }

// Bad: testing by spinning up a mock HTTP server just to check JSON parsing
```

---

### 2. Review Duration Tracking

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit for the timing wrapper; integration via CLI for end-to-end. |
| **Mock/Real** | Real `Instant::now()` -- do NOT mock the clock for this. |
| **Key anti-patterns** | **#7 (Flaky Tests):** NEVER assert `elapsed > 100ms` or similar timing thresholds. Tests will flake on CI. **#5 (Testing Implementation):** Don't verify that `Instant::now()` was called twice. |
| **Rust pitfall** | `Instant` is monotonic and fast -- no need to abstract it. Just test that the returned `Duration` is `>= Duration::ZERO` (sanity) or test the *struct that carries it*. |
| **TDD approach** | Test-after is fine. The logic is `start = now(); work(); elapsed = start.elapsed()` -- there's nothing to TDD. Focus your test on the **struct/return type** that carries the duration, not the measurement itself. |

```rust
// Good: test the data structure
assert!(result.duration >= Duration::ZERO);
assert!(result.duration < Duration::from_secs(60)); // sanity bound

// Bad: assert_eq!(result.duration, Duration::from_millis(142));
```

---

### 3. Suppression Counter

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit only. Already has the right shape -- `CalibrationResult` returns `suppressed: usize`. |
| **Mock/Real** | Real calibrator logic with synthetic `FeedbackEntry` vectors. |
| **Key anti-patterns** | **#4 (Wrong Functionality):** The counter already exists in `CalibrationResult.suppressed`. If you're just *exposing* it, don't write new unit tests for existing behavior -- that's testing what's already tested. Only test the NEW path (e.g., aggregating across files, formatting for display). **#6 (Coverage Theater):** Don't add a test that `suppressed == 0` when no feedback exists just to pad coverage. |
| **Rust pitfall** | None significant. This is straightforward plumbing. |
| **TDD approach** | TDD the aggregation/display logic if any. Skip TDD for simple field pass-through. |

---

### 4. Numeric Formatting Helper

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit only. Pure function, ideal for exhaustive table-driven tests. |
| **Mock/Real** | No dependencies. Pure `fn format_number(n: u64) -> String`. |
| **Key anti-patterns** | **#11 (TDD as Religion):** This IS the right place for strict TDD. Write the test table first, implement second. **#4 (Wrong Functionality):** Test the boundaries that matter: 0, 999, 1000, 1050, 999_999, 1_000_000, 1_500_000, u64::MAX. |
| **Rust pitfall** | Floating-point formatting: `format!("{:.1}", 63100.0 / 1000.0)` gives `"63.1"` but `format!("{:.1}", 1000.0 / 1000.0)` gives `"1.0"` not `"1"`. Decide up front: `"1.0k"` or `"1k"`? TDD forces this decision early. |
| **TDD approach** | **Strongly recommended.** Write a `#[test_case]` or manual table: |

```rust
// TDD test table -- write this FIRST
#[test]
fn format_number_cases() {
    let cases = [
        (0, "0"),
        (999, "999"),
        (1_000, "1.0k"),
        (1_050, "1.1k"),    // or "1.0k"? Decide now.
        (63_100, "63.1k"),
        (999_999, "1000.0k"), // or "1.0M"? Decide now.
        (1_000_000, "1.0M"),
        (1_500_000, "1.5M"),
    ];
    for (input, expected) in cases {
        assert_eq!(format_number(input), expected, "format_number({input})");
    }
}
```

---

### 5. Telemetry Module (JSONL Append)

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit for serialization, integration for file I/O. Follow the `FeedbackStore` pattern exactly. |
| **Mock/Real** | Real filesystem via `tempfile::TempDir`. Do NOT mock `std::fs`. |
| **Key anti-patterns** | **#9 (Test Code as Second-Class):** Extract a `TelemetryEntry` builder or test helper. The feedback tests already do this (`fn entry(model, verdict) -> FeedbackEntry`). Follow the pattern. **#7 (Flaky Tests):** If you record timestamps, inject `chrono::Utc::now()` or accept it as a parameter. Do NOT assert exact timestamps. **#10 (Not Converting Bugs to Tests):** JSONL corruption (truncated writes, concurrent appends) will be a real bug source -- write tests for malformed line handling NOW, before it bites you. |
| **Rust pitfall** | `serde_json::to_string` can fail if your struct contains non-serializable types (e.g., `PathBuf` with invalid UTF-8 on some platforms). Test round-trip: serialize then deserialize and assert equality. The feedback store already learned this lesson (`malformed entries are skipped`). |
| **TDD approach** | TDD the entry struct + serialization. Test-after for the file append mechanics (copy from feedback.rs). |

```rust
// Must-have test: round-trip serialization
fn test_telemetry_round_trip() {
    let entry = TelemetryEntry { ... };
    let json = serde_json::to_string(&entry).unwrap();
    let parsed: TelemetryEntry = serde_json::from_str(&json).unwrap();
    assert_eq!(entry, parsed);
}

// Must-have test: malformed line resilience
fn test_load_skips_malformed_lines() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("telemetry.jsonl");
    fs::write(&path, "{\"valid\":true}\ngarbage\n{\"valid\":true}\n").unwrap();
    let entries = TelemetryStore::load(&path).unwrap();
    assert_eq!(entries.len(), 2);
}
```

---

### 6. Stats Subcommand

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit for computation (extend `analytics.rs`), integration via `tests/cli.rs` for `quorum stats`. |
| **Mock/Real** | Unit: synthetic data vectors. Integration: real tempdir with fixture JSONL files. |
| **Key anti-patterns** | **#3 (Wrong Proportions):** This is a CRUD-read feature -- it reads files and formats output. Heavy unit testing of the formatting is **anti-pattern #4** (testing trivial code). Focus unit tests on the *aggregation logic* and use one integration test to verify the CLI wiring. **#1 (Unit Without Integration):** You MUST add a CLI integration test. The existing `tests/cli.rs` pattern makes this trivial. |
| **Rust pitfall** | The stats subcommand needs to find `~/.quorum/feedback.jsonl`. In tests, override via `HOME` env var (existing pattern in `cli.rs`). Don't hardcode paths. |
| **TDD approach** | TDD the new aggregation functions (e.g., telemetry stats). Test-after for CLI integration. |

```rust
// Integration test (add to tests/cli.rs)
#[test]
fn stats_with_empty_feedback() {
    quorum().arg("stats").assert().success()
        .stdout(predicate::str::contains("No feedback"));
}
```

---

### 7. Compact Output Formatter

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit only. Pure `fn format_compact(findings: &[Finding]) -> String`. |
| **Mock/Real** | No dependencies. Construct `Finding` structs directly. |
| **Key anti-patterns** | **#5 (Testing Implementation):** Test the output STRING, not how it was built. Don't assert on intermediate buffer state. **#9 (Second-Class Test Code):** Extract a `fn test_finding(title, severity) -> Finding` helper. Multiple test files will need this. Put it in `test_support.rs`. |
| **Rust pitfall** | `Finding` likely has many required fields. If constructing test `Finding` values is painful, add `impl Finding { #[cfg(test)] pub fn test_stub(title: &str) -> Self { ... } }` or use the builder pattern. Do this BEFORE writing formatter tests. |
| **TDD approach** | TDD is ideal here. Write the expected compact output first, then implement the formatter to match. |

```rust
// TDD: define the contract first
#[test]
fn compact_format_single_finding() {
    let f = Finding::test_stub("unsafe block without comment")
        .with_severity(Severity::High)
        .with_line(42);
    let output = format_compact(&[f]);
    assert_eq!(output, "src/main.rs:42 [HIGH] unsafe block without comment\n");
}
```

---

### 8. Cost Estimation (Pricing Lookup Table)

| Aspect | Recommendation |
|--------|---------------|
| **Test type** | Unit only. Pure lookup + arithmetic. |
| **Mock/Real** | No dependencies. Static pricing data. |
| **Key anti-patterns** | **#5 (Testing Implementation):** Test `estimate_cost(model, prompt_tokens, completion_tokens) -> f64`, NOT the internal HashMap structure. **#4 (Wrong Functionality):** Test the INTERESTING cases: unknown model fallback, zero tokens, very large token counts (overflow?), models with different input/output pricing. **#12 (Not Reading Docs):** If you use a TOML/JSON config for prices, use `serde` deserialization tests, not hand-rolled parsing. |
| **Rust pitfall** | Floating-point comparison: use `assert!((result - expected).abs() < 0.0001)` or the `approx` crate, NEVER `assert_eq!` on f64. Token counts are `u64` but pricing involves `f64` -- test the multiplication boundary (`u64::MAX as f64` loses precision). |
| **TDD approach** | TDD the lookup and calculation. Write failing tests for known model prices, then implement. |

```rust
// NEVER do this with floats
assert_eq!(estimate_cost("gpt-5.4", 1000, 500), 0.0325);

// Do this instead
let cost = estimate_cost("gpt-5.4", 1000, 500);
assert!((cost - 0.0325).abs() < 1e-6, "cost was {cost}");
```

---

## Cross-Cutting Anti-Pattern Risks

| Anti-Pattern | Risk Level | Where It Applies | Mitigation |
|---|---|---|---|
| #1 Unit without Integration | **High** | Features 1, 5, 6 | Add CLI integration tests for stats subcommand; telemetry file creation |
| #3 Wrong Proportions | Medium | Feature 6 | Stats is CRUD-read: 1-2 unit tests for aggregation, 1 CLI integration test. Don't over-unit-test formatting. |
| #4 Wrong Functionality | **High** | Features 2, 3 | Duration tracking and suppression counting are trivial plumbing. Don't write 10 tests for field pass-through. Focus on features 1, 4, 5, 8 where logic lives. |
| #5 Testing Implementation | **High** | Features 1, 7, 8 | Extract pure functions. Test input->output, never internal call sequences. |
| #7 Flaky Tests | **High** | Features 2, 5 | Never assert exact timing. Inject or ignore timestamps. |
| #9 Second-Class Test Code | Medium | Features 5, 6, 7 | Extract `TelemetryEntry` builder and `Finding::test_stub()` into `test_support.rs`. |
| #10 Bugs to Tests | Medium | Feature 5 | Malformed JSONL handling -- write the test before the bug finds you. |
| #11 TDD as Religion | Medium | Feature 2 | Don't force TDD on `Instant::now()` wrappers. TDD shines on features 4 and 8. |
| #12 Not Reading Docs | Low | Feature 7 | If adding `--compact` flag, check `clap` derive macro docs for output format args. |

## TDD Suitability Summary

| Feature | TDD? | Rationale |
|---------|------|-----------|
| 1. Token extraction | **Yes** | Pure parsing, clear contract, edge cases matter |
| 2. Duration tracking | No | Trivial wrapper, test-after for struct shape |
| 3. Suppression counter | No | Mostly plumbing of existing `CalibrationResult.suppressed` |
| 4. Numeric formatting | **Yes** | Textbook TDD: table-driven, pure function, boundary-heavy |
| 5. Telemetry module | **Partial** | TDD the struct+serde; test-after for file I/O (copy feedback.rs pattern) |
| 6. Stats subcommand | **Partial** | TDD new aggregation; test-after for CLI wiring |
| 7. Compact formatter | **Yes** | Define output contract first, implement to match |
| 8. Cost estimation | **Yes** | Pure arithmetic, float edge cases, unknown model fallback |
