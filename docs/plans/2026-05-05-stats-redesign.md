# `quorum stats` Dashboard Redesign

**Date:** 2026-05-05
**Author:** James + Claude (Opus 4.7), reviewed by GPT-5.4 + Opus 4.5
**Goal:** make stats interpretable — stable signal even under biased feedback, with gaps surfaced rather than hidden.

## Problem (after consensus pass)

The current dashboard mixes incompatible signals into a single precision number:

- Verdicts from **Human**, **PostFix**, **External agents**, **AutoCalibrate**, and legacy **Unknown** rows are summed into one corpus precision and one trend line.
- External agents (pal, coderabbit, codex) are **a different kind of signal entirely** — they reflect the bug-surface seen by other tools and serve as corpus expansion + AST-rule-mining queue, not as verdicts on quorum's own findings.
- AST rule findings have **no per-rule attribution** in `FeedbackEntry`. With 53 bundled rules we can't tell which produce signal vs. noise, can't auto-deprecate, can't justify additions.
- Trend movements (e.g. `70%→85%`) are uninterpretable: could be 5 entries flipping in a low-volume week, or a real shift. No sample size, no confidence band, no capture-rate footing.

GPT-5.4 (9/10 conf): *"Without durable finding_id, the redesign risks being more honest in wording, still wrong in math."*

## Disposition flow (the conceptual fix)

Each **finding** (run_id × finding_id) collects signals from multiple channels but has one final disposition:

```
Human verdict?       → use it (only source of FP/Partial/Wontfix nuance)
else PostFix exists? → TP (user fixed it)
else                 → unlabeled (counts toward capture rate, not precision)
```

**External lives outside this flow.** It's tracked separately as: corpus contribution count, agreement/disagreement rate with quorum's verdicts, top agents. Not aggregated into precision.

**AutoCalib stays excluded** from headline precision (already correct).

## Phase 0 — Schema + linkage audit (BLOCKING)

Both reviewers flagged this as the real prerequisite. Cannot ship per-finding precision without it.

| Task | Detail |
|------|--------|
| Audit `run_id` linkage | Measure join rate between `reviews.jsonl` and `feedback.jsonl`. Target ≥85% (GPT-5.4) / ≥70% (Opus). |
| Add `finding_id` to FeedbackEntry | Forward-only. Stable identity for dedup. |
| Add `rule_id` to FeedbackEntry | Forward-only. Foundation for AST rule scoring. |
| `quorum stats --join-health` | Diagnostic surface for linkage rate. |
| Extend `StatsReport` struct | Add: per-finding deduped counts, linkage rate, capture rate, Wilson CI metadata, separate external-overlap fields. |

If linkage <85%: ship Phase A as-is (no per-finding math), label headline trend as "entry-level (legacy)". Don't promote to per-finding precision until backfill is done.

## Phase A — Presentation + measurement primitives (independently shippable)

Pure cosmetic + Wilson CI. No data-model risk.

### Tier table → Channel attribution table

`format_tier_report` becomes attribution-only. No Prec column for External / AutoCalib (they don't measure quorum precision). Right-aligned numerics, single dim `─` rule under header, em-dash for empties.

```
Channel attribution
  Channel     Total    TP    FP  Part  Wfix
  ──────────  ─────  ────  ────  ────  ────
  Human       2,002  1047   409   348   198
  PostFix        45    45     —     —     —
  AutoCalib      27    10     4     8     5    (excluded from precision)
  Unknown       291     —     —     —     —    (legacy)
```

### Headline trend (single line, scope explicit)

```
Quorum precision (Human+PostFix, per-finding)
  Last 7d windows × 8:   77% [72-82]  →  76% [n=145]  ↑
                         oldest         current
                         capture: 18% labeled (212/1,159 findings)
```

- **Wilson interval** (consensus pick over bootstrap — simpler, stable for small-n proportions).
- **`n too low`** replacement when window <30 entries (don't render a misleading point).
- **Dim** windows where capture <10% rather than hide them (keeps history continuous).
- **Capture rate inline** so user sees the trend's footing without scrolling.

### External corpus block (separate section)

```
External corpus (last 7 days)
  Agent          Findings  Agreement   Disagreement   Corpus contribution
  ─────────────  ────────  ──────────  ─────────────  ───────────────────
  pal                 72        58%         14%             58 entries
  coderabbit          42        67%         12%             42 entries
  codex                2          —            —              2 entries (low sample)
```

Note: "agreement" = External called the same finding TP that quorum independently flagged. Real metric, not precision-aggregated with Human.

### Section labels normalized

`(7d)` → `(last 7 days)` everywhere. `Rolling 50 reviews` → `Rolling windows (50 reviews each)`. Sparkline legend at first appearance.

### Output budget

GPT-5.4 wins this disagreement. Default `quorum stats` capped at **~30 lines** per DESIGN.md restraint. By repo / by caller / rolling tables move to `quorum stats --full`. AutoCalib drift (`--trend=autocalib`) and per-tier decomposition (`--trend=per-tier`) behind flags.

## Phase B — Capture / bias surfacing (gated on Phase 0 linkage)

Only ship if Phase 0 linkage rate ≥85%.

```
Activity (last 7 days)
  Reviews: 138   Findings: 1,159   Findings/review: 8.4   Suppression: 1%
  Capture: 18% labeled (212/1,159)   Mix: TP 51% / FP 31% / Partial 18%
```

- No directional flag (the original `↑ possible negative bias` was inverted).
- Just report mix percentages and let user interpret.
- `Capture <10%` triggers a dim "low-coverage interpretation warning" sub-line.

## Phase C — AST rule attribution (gated on Phase 0 + sample accumulation)

Both reviewers want this in a dedicated command, not the main dashboard.

- New `quorum rules stats` command for per-rule precision tables.
- Default dashboard adds **one terse line**: `Rules: 31 attributed, 9 with n≥10; 3 under 40% precision`.
- Promote to default-dashboard block only after n≥10 per rule for ≥5 rules.

## DESIGN.md additions

**§4.x Tables** (consensus: tight enough as-is):
> Use a single dim `─` rule beneath the column header row only. Never above, beside, or below data rows. No box characters, no vertical separators. Numeric columns right-align to the value. Empty cells render as `—`.

**§12.x Trend interpretation** (softened per Opus 4.5):
> Trends should label **scope** (what's rolled in) and **unit** (`7d windows × N` or `50 reviews × N`) explicitly. Headline trend includes confidence interval (Wilson) when n≥30, and is replaced with `n too low` otherwise. Capture rate is shown inline so trend footing is visible.

## Explicitly skipped (by consensus)

- ~~Two-trend split~~ — one is enough; channel attribution table gives composition.
- ~~Inverse-propensity reweighting~~ — fragile, "pseudo-rigor" (GPT-5.4).
- ~~Bootstrap CI~~ — Wilson is simpler and more stable for proportions.
- ~~Stratified-by-severity headline~~ — drill-down only.
- ~~Auto-direction bias flag~~ — original phrasing was inverted; just show mix.
- ~~Hidden low-capture windows~~ — dim instead, keeps history continuous.
- ~~Weighted External-blended precision~~ — category error under softer name.

## Implementation surface

| Phase | Files | Estimated diff |
|-------|-------|---------------|
| 0 | `src/feedback.rs` (schema), `src/stats.rs` (audit), new `quorum stats --join-health` | ~250 lines |
| A | `src/analytics.rs` (Wilson CI, channel attribution), `src/stats.rs` (rendering, --full flag), `DESIGN.md` | ~300 lines |
| B | `src/analytics.rs` (capture stats), `src/stats.rs` (rendering) | ~100 lines |
| C | `src/main.rs` (new subcommand), `src/rule_stats.rs` (new), `src/ast_grep.rs` (rule_id propagation) | ~400 lines |

## Open questions

1. **Linkage backfill scope** — if Phase 0 audit shows poor linkage, do we backfill historical reviews.jsonl entries with synthetic finding_ids, or accept a gap and only measure forward?
2. **Wilson CI confidence level** — 95% is conventional; 90% gives tighter bands for the same data. Default to 95%, configurable?
3. **`quorum rules stats` shape** — separate command vs. flag on `quorum stats`? Both reviewers prefer separate command; user has not weighed in.

## Phasing summary

```
Phase 0 (BLOCKING)  — schema + linkage audit                     ~1 day
Phase A             — presentation + Wilson CI + channel attr     ~2 days
Phase B             — capture/bias (gated on Phase 0 ≥85%)        ~1 day
Phase C             — rule_id attribution + dedicated command     ~2 days
```

Phase A is safe to ship independently and answers the user's *primary* complaint (interpretability) even if Phases B/C slip.
