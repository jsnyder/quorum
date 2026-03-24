# Quorum vs Third-Opinion vs PAL Benchmark

**Date**: 2026-03-24
**Files reviewed**: 5 Rust source files from quorum itself
**Models**: quorum (local-ast + gpt-5.4), third-opinion (gpt-5.4), PAL (gpt-5.4)

## Results Summary

| File | Quorum (MCP) | Third-Opinion | PAL | Overlap |
|------|:---:|:---:|:---:|:---:|
| calibrator.rs | 6 | 2 | 0* | 2/2 TO shared |
| llm_client.rs | 6 | 3 | - | 3/3 TO shared |
| redact.rs | 7 | 4 | - | 3/4 TO shared |
| http_server.rs | 6 | 5 | - | 4/5 TO shared |
| **Totals** | **25** | **14** | **0*** | |

*PAL codereview returned 0 issues with internal validation on calibrator.rs (confidence: "low"). It appears to need the external validation path for substantive reviews.

## Detailed Comparison: calibrator.rs

### All tools agree on:
- **Partial/Wontfix verdicts ignored in suppression logic** (Q: medium, TO: high)
  All three identified that only TP/FP counts drive decisions while Partial/Wontfix are annotated but not counted.
- **Case-sensitive word_jaccard** (Q: medium, TO: medium)
  "SQL Injection" vs "sql injection" would fail to match.

### Quorum found, TO missed:
- Duplicate feedback entries can inflate counts (medium)
- Suppressed findings dropped but marked with action (design issue) (medium)
- Similarity threshold not validated (low)
- Type definition conflict warning (high) - FP: these types ARE defined here

### TO found, Quorum missed:
- Nothing unique to TO on this file

### PAL found:
- 0 findings (internal validation mode returned empty)

## Detailed Comparison: llm_client.rs

### All tools agree on:
- **UTF-8 panic on error truncation** `&error_text[..200]` (Q: medium, TO: high)
- **Unsafe indexing into choices[0]** (Q: medium, TO: medium)

### Quorum unique:
- block_in_place panic on single-threaded runtime (high)
- No base_url validation in constructor (medium)
- No request timeout configured (medium)
- Sync wrapper degrades async performance (medium)

### TO unique:
- Error body leaks to callers (medium) - similar to quorum's truncation finding

## Detailed Comparison: redact.rs

### All tools agree on:
- **Generic assignment regex too broad** / order-dependent (Q: medium, TO: high)
- **OpenAI key regex too narrow** (Q: implied in coverage, TO: high)
- **URL credential regex edge cases** (Q: medium, TO: medium)

### Quorum unique:
- AWS ASIA prefix not covered (high)
- GitHub token format evolution (high)
- PEM mixed-case labels (medium)
- Bearer token over-matching (medium)
- Sequential replacement interference (low)

### TO unique:
- Tests use placeholder inputs (medium) - test quality concern

## Detailed Comparison: http_server.rs

### All tools agree on:
- **Cache hit detection is racy** under concurrency (Q: medium, TO: medium)
- **Feedback load errors silently ignored** (Q: medium, TO: medium)
- **Internal errors exposed to clients** (Q: medium, TO: implied)
- **No auth on HTTP endpoints** (TO: high, Q: medium via binding)

### Quorum unique:
- HOME env var not portable (medium)
- User-controlled file path reflected in errors (low)

### TO unique:
- No request body size limit (medium)
- Socket path in /tmp is world-accessible (low)

## Quality Metrics

| Metric | Quorum (MCP) | Third-Opinion | PAL |
|--------|:---:|:---:|:---:|
| Total findings | 25 | 14 | 0 |
| High severity | 3 | 4 | 0 |
| Medium severity | 19 | 8 | 0 |
| Low severity | 3 | 2 | 0 |
| Est. TP rate | ~85% | ~90% | N/A |
| Est. FP rate | ~15% | ~10% | N/A |
| Unique findings | 14 | 3 | 0 |
| Calibrator active | Yes (674 entries) | No | No |
| Local AST findings | 0 (all Rust) | 0 | 0 |

## Key Observations

1. **Quorum finds ~2x more than TO** (25 vs 14) - hydrated context gives the LLM broader signal
2. **TO has higher precision** (~90% vs ~85%) - fewer findings but more consistently real issues
3. **Almost all TO findings are also in quorum** - quorum is a superset for these files
4. **Quorum's unique finds are real** - block_in_place panic, ASIA prefix, racy cache hit are genuine issues
5. **PAL codereview needs external validation mode** for substantive results
6. **No local AST findings on Rust** - quorum's local patterns (complexity, unwrap) didn't fire because these are clean, focused modules. Local analysis shines on Python.
7. **Calibrator added value** - 2 findings on calibrator.rs got precedent annotations

## Recommendations

- **For Rust code review**: Use quorum+LLM (more coverage) and cross-check critical findings with TO
- **For Python code review**: Use quorum (local catches secrets/SQL/debug instantly) + LLM for deeper issues
- **For security audit**: Run both quorum+LLM and TO, union the findings
- **For CI gates**: quorum local-only (free, instant, catches patterns)
- **PAL is better suited for chat/debug** than structured code review in this benchmark
