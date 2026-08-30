# Three-way comparison — `src/review.rs`

**Date**: 2026-04-26
**Target**: `src/review.rs` (1450 LOC; expanded in PR #81 async-permit fix)
**Tools**: quorum (gpt-5.4) · third-opinion · pal/chat (gpt-5.4)

## Headline

| Tool | Findings | Wall time | Unique HIGH | Severity calibrated |
|---|---|---|---|---|
| quorum | 5 | 47.0s | 1 (CC=22 + fence-strip bug) | yes (precedent-aware) |
| third-opinion | ~5 | ~comparable | 0 | no |
| pal (chat, gpt-5.4) | 5 | ~12s | 2 (hydration sandbox, repair drift) | no |

## Findings (deduped, severity normalized to my triage)

| # | Finding | quorum | third-opinion | pal | My verdict |
|---|---|:-:|:-:|:-:|---|
| 1 | `LlmFinding::into_finding`: unknown severity silently → `Info` (line 46-53) | — | M | M | TP — fix to `Medium` + log |
| 2 | `extract_json_array`: `trim_end_matches("```")` strips trailing backticks from JSON string values (line ~360) | M (confirmed) | — | — | TP — quorum's unique catch, real correctness bug |
| 3 | Hydration `callers`/`callee_signatures` use prose bullets, not sandbox/fence wrappers (line 101-130) | — | partial (test-gap framing) | H | TP — PAL's unique, real prompt-injection surface |
| 4 | `sanitize_json_escapes` silently mutates LLM output (line 306-359) | M (CC only) | L (re-parse perf) | H (semantic drift) | Partial — repair is intentional, but lack of "degraded" flag is real |
| 5 | No bounds clamp of `line_start`/`line_end` to file length (line 60-61) | — | — | M | TP — PAL's unique, exploitable for inflated reports |
| 6 | `build_review_prompt` cyclomatic complexity 22 (line 82-200) | H (precedent: partial/wontfix) | — | — | Wontfix — coherent state machine, matches prior triage |
| 7 | `sanitize_json_escapes` cyclomatic complexity 20 (line 307-359) | M (precedent: wontfix sim=1.00) | — | — | Wontfix — already triaged on prior PR |
| 8 | `parse_llm_response` reparses overlapping payload variants | — | L | — | Partial — perf only, low priority |
| 9 | Unbounded prompt size under parallel review | — | — | L/M | Wontfix-leaning — caller-side concern |

## Per-tool strengths

**quorum**
- Only tool to find the **fence-strip backtick-deletion bug** (real correctness)
- Calibrator surfaced precedent for both CC findings → human can see prior triage at a glance
- Linter integration (clippy enabled, others auto-detected)
- 47s wall time with full hydration + precedent retrieval is competitive given 1450 LOC + LLM round-trip

**third-opinion**
- Caught the severity-downgrade issue independently (same as PAL → corroborating signal)
- Notes prompt-injection test-coverage gap (constructive, not just bug-hunting)
- Conservative: no false-positive HIGHs on this clean module
- Lower hit rate on novel issues — felt like quorum-without-calibrator

**pal (gpt-5.4 via chat)**
- **Best at HIGH-severity reasoning**: only tool to surface the hydration-sandbox gap and the line-bounds clamping issue
- Frames `sanitize_json_escapes` as a *trust* problem (semantic drift), not a *complexity* problem — different lens from quorum
- No precedent retrieval / no calibration — would burn FP budget on noisier files
- Direct chat invocation (`mcp__pal__chat`) is far better than `mcp__pal__codereview` workflow for one-shot review (codereview is multi-step orchestration)

## Overlap matrix

```
                quorum    3o      pal
quorum            5       0        0
3o                0       5        1   (severity downgrade)
pal               0       1        5
```

Cross-tool corroboration: only **1** finding (severity downgrade). Each tool has a distinct lens. **All three together** > any pair.

## Recommendation

- **CI gate**: quorum local-only or quorum+LLM (calibrated; precedent-aware)
- **Pre-merge deep audit**: quorum + PAL chat in parallel (PAL's HIGH-reasoning complements quorum's calibrated catches)
- **Skip**: PAL's `codereview` MCP workflow for ad-hoc reviews — too many round-trips. Use `chat` with file paths.
- **Feedback loop**: record the unique PAL finds (hydration sandbox, line-bounds clamp) into `~/.quorum/feedback.jsonl` as TP examples — calibrator will pick up the patterns next time.

## Action items surfaced

1. **HIGH**: wrap `<hydration_context>` callers/signatures in explicit untrusted-data fence (PAL #1) → filed as #112, **but see meta-review below — defer until PoC or corroboration**
2. **MEDIUM**: clamp `line_end` to actual excerpt length in `into_finding` (PAL #3) → filed as #113
3. **MEDIUM**: change `_ => Severity::Info` → `Severity::Medium` + tracing::warn on unknown severity (3o + PAL)
4. **MEDIUM**: fix `extract_json_array` fence stripping to one-shot `strip_suffix` (quorum #3)
5. **LOW**: flag findings produced after `sanitize_json_escapes` mutation as `degraded`/`repaired` (PAL #2)

---

## Meta-review (2026-04-26 evening)

I asked two frontier models from different lineages to critique the comparison itself:

- **gpt-5.2-pro** (OpenAI, via direct LiteLLM)
- **claude-opus-4.5** (Anthropic, via OpenRouter)

(Originally wanted Gemini-3.1-pro-preview but the direct Gemini key in PAL's env was expired — caught and fixed: PAL is now configured to route through LiteLLM. See `pal_mcp_litellm_routing.md`.)

### Consensus across both meta-reviewers

Both, independently, hit the same core critiques:

1. **n=1 is insufficient for tool characterization.** Per-tool labels (`quorum=calibrated`, `3o=conservative`, `PAL=HIGH-reasoning`) are pattern-matching on noise. Need ≥5 files across complexity profiles before labels stick.

2. **Model overlap is a confound.** quorum and PAL both used gpt-5.4 — differences attributed to "calibration vs raw reasoning" could be prompt framing or context utilization. Need a quorum-without-calibrator baseline on the same file to isolate the calibrator's contribution.

3. **Triage isn't blind.** Author labeled severity knowing which tool said what — confirmation bias baked in. For TP/FP claims, need at least one independent rater labeling without tool attribution.

4. **`#112 was filed prematurely.`** PAL's hydration-sandbox HIGH has zero cross-tool corroboration and no demonstrated exploit. Both reviewers flagged this as overweighting one LLM. **Recommended**: move `#112` to a needs-validation queue pending PoC (e.g., a unit test with a crafted signature like "Ignore previous instructions") or 2nd-tool corroboration.

5. **`#113 is defensible`** — concrete logic gap, lower stakes, even if severity is debatable.

6. **Double-recording corroborated findings does skew retrieval.** Recording the severity-downgrade catch as TWO External entries (one 3o, one pal) at 0.7x each gives effective weight 1.4x — higher than a single Human entry. **Better design**: one canonical entry with `corroborated_by: [3o, pal]` field, retrieval scores as `0.7 * (1 + 0.3 * num_corroborators)` (diminishing returns).

### Where they diverged

Almost nowhere — the agreement was striking. claude-opus-4.5 was sharper on the calibrator-feedback math (exact formula). gpt-5.2-pro was sharper on the "disconfirm step" (explicitly try to falsify PAL's hydration claim before filing).

### Implications for this artifact

This comparison is a **first impression**, not a defensible study. The action items remain useful; the per-tool conclusions don't generalize.

### What a defensible follow-up looks like

1. **5-file panel**, sampled across: high-churn, legacy, greenfield, test-heavy, security-sensitive. Avoid files from PRs the author just shipped.
2. **Blind labeling**: anonymize tool attribution before triage; ideally a second rater.
3. **Quorum-without-calibrator baseline** on each file to isolate the calibrator contribution.
4. **Pre-filing rule**: HIGH security findings require ≥2 tool corroboration OR a PoC. Singleton HIGHs go to a `needs-validation` queue.
5. **Calibrator schema change**: dedupe to canonical entries with a `corroborated_by` array; retrieval boosts with diminishing returns rather than additive External entries.
