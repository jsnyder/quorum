# External Comparison Corpus

Clean test fixtures for cross-tool comparisons. **Do not record quorum feedback
for findings on these files** — they must remain uncontaminated for future A/B
testing.

## Samples

Real-world open-source files covering Rust, Python, and TypeScript:

| File | Source | Lines | Why chosen |
|------|--------|-------|------------|
| `index_writer.rs` | tantivy search engine | 2588 | Concurrency, thread pools, channel patterns |
| `client.py` | httpx HTTP client | 2019 | Async/sync duality, transport lifecycle, redirects |
| `router.ts` | tRPC framework | 565 | Type guards, recursive construction, lazy loading |

## Running a comparison

```bash
# 1. Quorum
quorum review tests/fixtures/comparison/samples/*.rs tests/fixtures/comparison/samples/*.py tests/fixtures/comparison/samples/*.ts --json > /tmp/cmp-quorum.json

# 2. PAL (via MCP)
# Use mcp__pal__codereview on each file

# 3. Third-opinion
third-opinion review tests/fixtures/comparison/samples/index_writer.rs --focus security,correctness > /tmp/cmp-to-rust.json
third-opinion review tests/fixtures/comparison/samples/client.py --focus security,correctness > /tmp/cmp-to-python.json
third-opinion review tests/fixtures/comparison/samples/router.ts --focus security,correctness > /tmp/cmp-to-ts.json
```

Then compare against the baseline in `baselines/`.

## Baselines

Each baseline records the exact findings from all three tools at a point in time.
Compare new runs against the baseline to measure regressions or improvements.

- `2026-05-10-precision-targeting.json` — first baseline, taken during Context7
  precision-targeting PR #287. Quorum 19, PAL 9, Third-Opinion 10.

## Rules

1. Never record quorum feedback (`quorum feedback`) for these files
2. Always use `--json` for quorum to get structured output
3. Record the quorum version/commit, model, and third-opinion version in each baseline
4. When upstream files change significantly, fetch fresh copies and start a new baseline series
