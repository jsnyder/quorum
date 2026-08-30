# Reliability & Correctness Bugfix Rollup

**Issues:** #374, #353, #289, #432

## Bug 1: debug_assert hardening (#374)

**File:** `src/logistic.rs:77,140`

`debug_assert_eq!` and `debug_assert!` guard dimension validation in `predict_one` and `fit`. These are stripped in release builds, causing silent index-out-of-bounds panics instead of clear assertion failures.

**Fix:**
- Replace `debug_assert_eq!` with `assert_eq!` in `predict_one` (line 77)
- Replace `debug_assert!` with `assert!` in `fit` (line 140)
- Add `assert!(lambda >= 0.0, "lambda must be non-negative")` in `fit` — negative lambda makes the L2 objective non-convex

## Bug 2: embed_batch length validation (#353)

**File:** `src/embeddings.rs:44-46`

`embed_batch` passes through `self.model.embed(texts, None)` without verifying the returned vector count matches input count. If fastembed silently drops or duplicates entries, callers get misaligned results.

**Fix:**
- Add early return for empty input (`texts.is_empty()` -> `Ok(Vec::new())`)
- Validate `results.len() == texts.len()` after calling the external library
- Use `anyhow::bail!` (not `assert!`) since this is a runtime condition from an external dependency

## Bug 3: --rev silently ignored with --path (#289)

**File:** `src/main.rs:594-605`

When `--path` is provided with `--rev`, the `AddLocation::Path(p)` variant drops `a.rev` silently. The user likely expects `--rev` to apply to the path.

**Fix:**
- Add a match arm for `(Some(_), None)` when `a.rev.is_some()` that returns an error: `"--rev may only be used with --git, not --path"`
- Error is better than warning — fail fast on invalid input

## Bug 4: temperature rejected by reasoning models (#432)

**File:** `src/llm_client.rs:785,949,1026,1096`

Quorum hardcodes `temperature: 0.3` (or `0` for judge) in all request bodies. OpenAI reasoning models (o1, o3, o4, gpt-5.x) reject temperature, returning HTTP 400.

**Fix:**
- Add `fn supports_temperature(model: &str) -> bool` that returns `false` for reasoning models
- Detection: prefix match on `o1`, `o3`, `o4`, `gpt-5` after `to_ascii_lowercase()`
- Conditionally omit `temperature` in all 4 request builders: `chat_completion`, `judge_completion`, `responses_api`, `chat_with_tools`
- Existing codex handling (`if !model.contains("codex")`) in `responses_api` is subsumed by this

## Closed issues

- **#394** (empty expect messages): already fixed in PR #402
- **#392** (discarded Results in main.rs): already fixed in PR #402
- **#393** (discarded Result in builder.rs): already fixed in PR #402
- **#391** (block_on in async): false positive — call is inside `spawn_blocking` (separate thread pool), which is correct per Tokio docs

## Testing strategy

Each bug gets at least one regression test:
- **#374:** Unit tests for `predict_one` with wrong dimensions (should panic), `fit` with ragged matrix (should panic), `fit` with negative lambda (should panic)
- **#353:** Unit test for empty input (returns empty vec), unit test with mock/spy verifying length check fires
- **#289:** CLI integration test or unit test verifying `--path` + `--rev` returns exit code 2
- **#432:** Unit tests for `supports_temperature` covering gpt-5.x, o1, o3, o4, gpt-4o, claude models. Integration test verifying request body omits temperature for reasoning models
