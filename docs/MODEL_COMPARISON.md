# Model Comparison for Code Review

**Date**: 2026-03-25
**Test file**: nodriver_spider.py (705 lines, Scrapy + nodriver browser automation)
**All models**: same prompt, same file, temperature 0.3, max_tokens 16384

## Finding Counts

| Model | Type | Findings | High+ | Medium | Low/Info | Time |
|-------|------|:---:|:---:|:---:|:---:|:---:|
| gpt-4.1 | Non-reasoning | 12 | 0 | 5 | 7 | 23s |
| gpt-4.1-mini | Non-reasoning (small) | 13 | 1 | 7 | 5 | 30s |
| **gpt-5.4** | **Thinking** | **16** | **3** | **11** | **2** | **25s** |
| gpt-5-mini | Thinking (small) | 9 | 2 | 4 | 3 | 52s |
| o3 | Deep reasoning | 5 | 2 | 2 | 1 | 21s |
| gemini-2.5-pro | Thinking | 6 | 2 | 2 | 2 | 52s |
| claude-sonnet-4-6 | Thinking | 18 | 4 | 9 | 5 | 51s |

## Key Findings by Model

### Only gpt-5.4 and claude found:
- Tab leak on exceptions (high)
- Timed-out future not cancelled (high)
- Race condition in browser init (medium)

### Only claude found:
- URL typo in product paths (medium)
- CSS :contains() not valid in cssselect (medium)
- Event loop closed while coroutines pending (medium)

### Only o3 found:
- Invalid Config.add_argument usage (high)

### gpt-4.1 missed:
- All high-severity findings (0 highs out of 12 findings)
- Tab leak, reactor block, web security — all real bugs

## Precision vs Recall Analysis

```
                Higher Recall →
            5       10       15       18 findings
  High  o3 ●──────────────────────────────────
  Prec.     gemini ●
               gpt-5-mini ●
                     gpt-4.1 ●
                          gpt-5.4 ●
  Low                          claude ●
  Prec.
```

- **o3**: 5 findings, all actionable — "precision sniper"
- **gpt-4.1**: 12 findings, high precision but misses severity
- **gpt-5.4**: 16 findings, best balance of recall + real bugs
- **claude**: 18 findings, most thorough but more to triage

## o3 as Calibrator

gpt-5.4 review (16 findings) + o3 auto-calibration:

| o3 Verdict | Count | % | Examples |
|---|:---:|:---:|---|
| TP | 22 | 61% | Tab leak, reactor block, bare excepts |
| Partial | 9 | 25% | Headful mode (intentional), race condition (low concurrency) |
| FP | 3 | 8% | Unused import, logger style |
| Wontfix | 2 | 6% | disable_web_security (scraper context) |

o3 shows better contextual judgment than self-calibration:
- Rates `disable_web_security` as **wontfix** (understands scraper context)
- Rates `headful mode` as **partial** (may be intentional for anti-bot)
- Confirms tab leak and reactor block as **TP** with specific reasoning

## Recommended Configuration

```bash
# Default: gpt-5.4 review + o3 calibration
quorum review src/file.py --calibration-model o3

# Budget: gpt-5-mini review + no calibration
quorum review src/file.py --no-auto-calibrate

# Deep audit: claude-sonnet review + o3 calibration
QUORUM_MODEL=claude-sonnet-4-6 quorum review src/file.py --calibration-model o3
```

## Cross-Language Validation

Tested gpt-5.4 + o3 calibration on:
- **Python** (nodriver_spider.py, server.py): 24 findings, 16 verdicts
- **TypeScript** (AuthMiddleware.ts): 7 findings, 7 verdicts
- **Rust** (calibrator.rs): 5 findings, 5 verdicts

o3 calibration consistent across all languages.
