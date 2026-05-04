# Fuzzy Title Matching in Calibrate Corpus Join — Design

## Problem

`quorum calibrate` joins `feedback.jsonl` with `calibrator_traces.jsonl` on
`(finding_title, file_path)`. LLM-generated finding titles are non-deterministic
across review runs, so the same issue gets different titles each time.

Current join rate: 329/2,492 (13%). Root cause: 1,603 unique feedback titles have
no exact match in traces.

## Evidence

Title variation patterns observed:
- **Backtick formatting**: `Atomic JSON writer uses a fixed .tmp filename` vs
  `Atomic JSON writer uses a fixed \`.tmp\` filename` (51 titles)
- **Rule-name prefix**: `Empty .expect() message` vs
  `expect-empty-message: Empty \`.expect()\` message`
- **Sentence extension**: `Reset can race with visit processing` vs
  `Reset can race with visit processing and lose the cleaned state`
- **Rephrasing**: `Background LLM judge tasks are not tracked` vs
  `Detached LLM judge tasks are never tracked or cancelled`

Jaccard score distribution (best match per unmatched feedback title):
- `>= 0.5`: 202 titles (12.6%) — spot-check shows mostly correct
- `0.35-0.5`: 136 titles — noisy, many false matches
- `< 0.35`: 1,265 titles — genuinely different issues

## Design: Tiered Matching

Match feedback to traces in priority order, stopping at first match:

| Tier | Strategy | Precision | Expected Recovery |
|------|----------|-----------|-------------------|
| 1 | Raw exact `(title, file_path)` | Highest | Current 329 |
| 2 | Normalized exact `(norm_title, file_path)` | Very high | +50-80 |
| 3 | Fuzzy same-file (Jaccard >= 0.5, margin >= 0.1) | High | +80-120 |
| 4 | Normalized exact title-only (legacy fallback) | Medium | +20-40 |

**Deferred to v2**: Cross-file fuzzy (tier 5) — higher false-match risk, violates
the `title_only_blocked_when_file_scoped_exists` anti-contamination invariant.

### Title Normalization

```
fn normalize_title(raw: &str) -> String:
    1. Strip rule-name prefix: regex ^[a-z][a-z0-9-]+:\s*
    2. Lowercase
    3. Replace punctuation except _ with spaces
    4. Collapse whitespace
    5. Trim
```

### Token Jaccard

```
fn token_jaccard(a: &str, b: &str) -> f64:
    tokens_a = normalize_title(a).split_whitespace().collect::<HashSet>()
    tokens_b = normalize_title(b).split_whitespace().collect::<HashSet>()
    |intersection| / |union|
```

### Fuzzy Match Acceptance

A fuzzy match is accepted only when:
1. Best Jaccard score >= 0.5 (configurable constant, not user-facing)
2. Best score exceeds second-best by >= 0.1 (ambiguity margin)
3. Both tokens sets are non-empty

### Observability

Log match strategy counts via `tracing::info`:
- `exact_raw`, `exact_normalized`, `fuzzy_same_file`, `normalized_title_only`
- `ambiguous_skipped`, `below_threshold`, `unmatched`

### Invariants Preserved

- `title_only_blocked_when_file_scoped_exists` — normalized title-only fallback
  (tier 4) still blocked when file-scoped traces exist for the same normalized title
- Ambiguous keys (duplicate traces for same join key) still removed
- Wontfix/unknown verdicts still filtered
- Negative weights still clamped to zero

## Reviewed By

GPT-5.4 (2026-05-04): Endorsed tiered approach. Key additions: normalized-exact
tier before Jaccard, same-file fuzzy before cross-file, best-vs-second margin
framing, punctuation normalization (not just backticks), match strategy logging.
