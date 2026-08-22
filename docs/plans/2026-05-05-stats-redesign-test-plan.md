# Stats Dashboard Redesign — Acceptance Criteria & Test Plan

**Companion to:**
- `2026-05-05-stats-redesign-implementation.md` (14-task plan)
- `2026-05-05-stats-redesign.md` (design doc)

**Scope:** Phase 0 (schema + linkage diagnostic) + Phase A (presentation + Wilson CI + per-finding precision).

**Status:** reconciled across two parallel agent reviews — `test-planning-implementation` (coverage gaps) + `testing-antipatterns-expert` (smell patterns). Antipattern-driven rewrites applied below; coverage additions kept.

## Reconciliation summary (changes from initial plan sketches)

**Dropped (antipattern: testing the framework / algorithm not contract):**
- Task 1 pure-roundtrip serde test → kept legacy-row test only
- Task 2 `centers_on_proportion`, `narrows_with_more_samples`, `handles_extremes` (algorithm properties, would pass on any correct Wilson impl) → replaced with ONE numerical pin against published reference value
- Task 6 tautology `x || !x` → replaced with concrete threshold test
- Task 11 / Task 14 full-output snapshots → replaced with structural assertions (section presence/order)

**Renamed (antipattern: implementation-tied test names):**
- Task 7 `per_finding_dedup_collapses_human_plus_postfix_on_same_finding` → `human_verdict_wins_when_same_finding_has_both_human_and_postfix`
- Task 7: all four tests renamed around precedence rules, not mechanism

**Tightened (antipattern: brittle visual / loose assertions):**
- Task 8 `rule_count >= 1 && rule_count <= 6` → `assert_eq!(rule_count, 1)`
- Task 8 `out.contains("100% prec")` negative → assert structural (column headers present/absent)
- Task 10 `contains("[71-81]") || contains("[71"))` → decide format first, test exact, OR test `format_ci_band` separately

**Added (coverage gaps from test-planning):**
- Task 1: `omits_finding_id_key_when_none` (disk-bloat regression — load-bearing for ~1,470 historical rows)
- Task 7: Wontfix + PostFix → Wontfix (silent precision inflation if wrong)
- Task 7: multi-Human tiebreak (latest-wins, pinned policy)
- Task 10: n=29 / n=30 boundary tests (the single most user-visible threshold)
- Task 14: empty-corpus rendering (no panic, no NaN, graceful degradation)
- Task 4: ReviewRecord finding_ids propagation invariants

**Inline mystery guests** (antipattern: hidden test inputs):
- Replace `Provenance::External { /* ... */ }` placeholders with actual minimal constructors before TDD starts
- Inline `write_test_reviews(&temp, /* ... */)` fixtures or use named builders

---

## Testing approach summary

This plan layers on top of the implementation plan's draft tests. The dominant risks are (1) JSONL forward-compat — `FeedbackEntry` is persisted, so deserialization of legacy rows and write-side bloat avoidance are load-bearing; (2) provenance precedence math, since per-finding dedup with Human > PostFix > drop is a multi-row collapse where the wrong winner silently flips precision; (3) confidence-band edges — Wilson at n=0, n=1, p=0, p=1, and the n=30 sample-size gate; (4) cross-task wiring — the `StatsReport` extension in Task 6 must be visibly populated by the rendering pipeline in Tasks 10/14, and the `headline_trend_uses_finding_id` flag gates a banner the user sees. We bias toward tests that exercise observable behavior at the `compute_report` / `format_human` boundary rather than internal helpers, with golden-output snapshots pinning the rendered dashboard structure.

---

## Per-task acceptance criteria & additional test cases

### Task 1 — `finding_id` / `rule_id` on `FeedbackEntry`

**Acceptance criteria**
- `FeedbackEntry` has new `Option<String>` fields `finding_id` and `rule_id`, both `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- Legacy JSONL rows (no `finding_id`, no `rule_id`) deserialize cleanly into `Some(entry)` with `finding_id == None && rule_id == None`.
- Writing an entry with `finding_id == None` produces JSON that does **not** contain a `"finding_id"` key (no `null` literal on disk).
- Writing an entry with `Some(id)` round-trips losslessly.
- Existing serde tests for `FeedbackEntry` still pass unmodified.

**Tests beyond the plan sketch**
- `feedback_entry_omits_finding_id_key_when_none` — serialize an entry with `finding_id = None`; assert the JSON string does not contain the substring `"finding_id"`. Prevents disk bloat across ~1,470 historical rows on rewrite.
- `feedback_entry_legacy_then_resave_preserves_absence` — read a legacy line, write it back, assert the rewritten line is byte-equal to the input modulo whitespace ordering (no `null` injected).
- `feedback_entry_rejects_nonstring_finding_id` — JSON like `{"finding_id": 42, ...}` must fail deserialization (catch accidental `Option<serde_json::Value>` regression).
- `feedback_entry_round_trip_with_only_rule_id` — `finding_id = None, rule_id = Some(...)` (forward-only AST-rule scoring case).

**Plan-test sufficiency:** the two sketched tests cover positive cases but miss the `skip_serializing_if` / null-bloat invariant — that's the single most important migration property and should be a required test.

---

### Task 2 — Wilson interval helper

**Acceptance criteria**
- `wilson_interval(successes, total, confidence) -> (f64, f64)` returns bounds in `[0.0, 1.0]`.
- `total == 0` returns `(0.0, 1.0)` (uninformative band).
- Width strictly decreases as `n` grows for a fixed proportion.
- Function is pure, no I/O, no panics on any `(successes ≤ total, total < usize::MAX)` input.

**Tests (revised after antipattern review)**

Antipattern expert flagged that 3 of 4 sketched tests verify Wilson algorithm properties (would pass on any correct impl). Replaced with one numerical pin + edge cases that exercise OUR contract decisions:

- **KEEP** `wilson_interval_with_zero_n_returns_unit_band` — our edge-case decision (uninformative band, not panic).
- **REPLACE** `centers_on_proportion`, `narrows_with_more_samples`, `handles_extremes` with one numerical pin:
  - `wilson_interval_n_60_p_0_5_matches_published_reference` — `(30, 60, 0.95)` should give `(0.376, 0.624)` ±0.005. One pin catches sign errors and wrong z constants without re-deriving the formula in test names.
- **ADD (our contract decisions, not algorithm):**
  - `wilson_interval_confidence_unknown_falls_back_to_95` — `confidence = 0.42`: bounds equal those for `0.95`. Locks our `z_score` fallback decision.
  - `wilson_interval_p_zero_at_small_n_lower_bound_is_zero` — `(0, 5, 0.95)`: lower exactly 0.0, upper < 1.0. Tests our clamp behavior.
  - `wilson_interval_p_one_at_small_n_upper_bound_is_one` — `(5, 5, 0.95)`: upper exactly 1.0. Same.
  - `wilson_interval_successes_exceed_total_is_handled` — debug-assert or `Err`; pin whichever the impl chooses.

**Plan-test sufficiency:** revised — algorithm verification dropped; only OUR contract is tested.

---

### Task 3 — `linkage_stats(reviews, feedback)`

**Acceptance criteria**
- Returns `LinkageStats { linked, unlinked }` plus `rate() -> f64` (0.0 when total == 0).
- `linked` = feedback entries whose `finding_id` is `Some(id)` AND `id` appears in some review's `finding_ids`.
- `unlinked` = everything else (legacy rows, dangling IDs, missing field).
- O(n+m) using a `HashSet<&str>` over known IDs.

**Tests beyond the plan sketch**
- `linkage_rate_with_zero_reviews_and_zero_feedback_returns_0_0_rate` — both empty: `linked=0, unlinked=0, rate()=0.0`.
- `linkage_rate_duplicate_finding_id_in_reviews_does_not_double_count` — two reviews carry `"finding-A"`; one feedback entry references it. `linked=1` (set semantics).
- `linkage_rate_duplicate_feedback_for_same_finding_counts_each` — two feedback rows reference `"finding-A"`. `linked=2` (we count entries, not findings; the dedup happens later in Task 7).
- `linkage_rate_empty_string_finding_id_is_unlinked` — defensive: `finding_id == Some("")` should not match an empty entry in the set if reviews contain no empty IDs.
- `linkage_rate_handles_large_corpus` — 100k feedback × 10k reviews completes in <100ms. Cheap perf guard.

**Plan-test sufficiency:** good baseline. The "100% linkage" and "0% linkage" boundary cases are missing, and matter because the diagnostic prints a different message at each.

---

### Task 4 — `ReviewRecord.finding_ids` propagation

**Acceptance criteria**
- `ReviewRecord` has `finding_ids: Vec<String>` with `#[serde(default)]`.
- Pre-rollout `reviews.jsonl` rows deserialize with `finding_ids == vec![]`.
- New review writes populate `finding_ids` from the review's emitted findings (in stable order, post-suppression).
- An empty `finding_ids` field is allowed and does not break stats computation.

**Tests beyond the plan sketch**
- `review_record_legacy_row_deserializes_with_empty_finding_ids`.
- `review_record_round_trip_preserves_finding_ids_order` — order matters for any future positional joins.
- `review_record_emits_finding_ids_only_for_non_suppressed_findings` — integration test on `run_review`: a finding suppressed by calibrator must still produce a stable ID (so feedback against it still links), OR explicitly excluded — pin the policy.
- `review_record_finding_ids_match_emitted_findings_count` — invariant: `finding_ids.len() == reviewed_findings.len()` post-suppression. Catches off-by-one when wiring.

**Plan-test sufficiency:** the plan flags this as a "blocking sub-task gated on inspection." It needs a definitive yes/no answer before any other Phase 0 task is testable end-to-end, since linkage and per-finding precision both depend on it.

---

### Task 5 — `quorum stats --join-health` flag

**Acceptance criteria**
- `quorum stats --join-health` short-circuits the normal report and prints a diagnostic block containing:
  - reviews count + total findings count,
  - feedback entry count split into linked / unlinked legacy,
  - linkage rate as a percentage,
  - a threshold annotation when `<85%` ("per-finding precision falls back to entry-level").
- Exit code is `0` even when linkage is low (it's a diagnostic, not a failure).
- `--join-health` is incompatible with `--full` (or composes cleanly — pin the choice).

**Tests beyond the plan sketch**
- `join_health_with_zero_feedback_emits_zero_linkage_rate` — empty `feedback.jsonl`, ensures we don't divide by zero.
- `join_health_at_exactly_85_percent_omits_warning` — boundary; warning should fire only `< 85%`, not `≤`.
- `join_health_at_84_percent_emits_warning` — paired with above.
- `join_health_with_corrupted_feedback_line_skips_and_continues` — one malformed JSONL line, rest still counted (matches the v0.9.1 "malformed entries are skipped" invariant).
- `join_health_does_not_print_normal_dashboard` — assert absence of "Channel attribution" / "Headline trend" headers in the output.

**Plan-test sufficiency:** the sketched test asserts substring presence. Add the threshold-boundary tests — these directly affect the user-visible warning.

---

### Task 6 — `StatsReport` extension

**Acceptance criteria**
- `StatsReport` gains: `linkage_rate`, `linkage_linked`, `linkage_unlinked`, `capture_rate`, `capture_labeled`, `capture_total`, `headline_trend_uses_finding_id`, `external_overlap`.
- `compute_report` populates all of them in one pass.
- Every new field has a defined default for the empty-corpus case.
- Backward-compat: any existing `StatsReport` consumer compiles unchanged (additive only).

**Tests beyond the plan sketch**
- `stats_report_empty_corpus_has_safe_defaults` — empty feedback + empty reviews: `linkage_rate=0.0`, `capture_rate=0.0`, `headline_trend_uses_finding_id=false`, `external_overlap` is empty.
- `stats_report_capture_rate_clamped_to_unit_interval` — labeled > total is impossible by construction; assert the impl returns 1.0 (or panics in debug) rather than > 1.0.
- `stats_report_headline_trend_uses_finding_id_true_iff_linkage_ge_85` — the flag's definition. Precise threshold matters for Task 10's banner.
- `stats_report_capture_total_counts_findings_not_entries` — under linked corpus, capture denominator is "findings in window," numerator is "findings with at least one Human|PostFix entry."

**Plan-test sufficiency:** the sketched test (`headline_trend_uses_finding_id || !headline_trend_uses_finding_id`) is a no-op — drop it. The threshold test above replaces it.

---

### Task 7 — Per-finding deduplication

**Acceptance criteria**
- `precision_trend_per_finding(entries, window_days)` produces one disposition per `finding_id` per window:
  1. If any Human entry exists for the finding in the window → use Human's verdict (FP/TP/Partial/Wontfix).
  2. Else if any PostFix entry exists → TP.
  3. Else → drop from the precision computation (counts toward capture, not precision).
- External and AutoCalib provenance are ignored for this computation.
- Legacy entries (finding_id = None) are skipped silently.
- Multiple Human entries for the same finding: latest by `timestamp` wins (pin policy).

**Tests (revised — names describe precedence rules, not dedup mechanism)**

- `human_verdict_wins_when_same_finding_has_both_human_and_postfix` — Human FP + PostFix TP → FP. (Replaces the original `per_finding_dedup_collapses_human_plus_postfix_on_same_finding`.)
- `human_wontfix_plus_postfix_resolves_to_wontfix_not_tp` — load-bearing for precision integrity; getting it wrong silently *inflates* precision.
- `human_partial_plus_postfix_resolves_to_partial`.
- `latest_human_verdict_wins_when_two_humans_disagree` — Human FP at t=1, Human TP at t=2 → TP. Pins "latest wins" policy.
- `external_provenance_does_not_count_toward_per_finding_precision` — single External TP for finding-A: trend window count=0.
- `autocalib_provenance_does_not_count_toward_per_finding_precision`.
- `legacy_entries_without_finding_id_are_skipped` — and assert telemetry/log captures the skip count (silent skip is itself a smell).
- `legacy_entry_for_same_title_does_not_pollute_modern_finding_id_dispositions` — same `(file, title)` matches a legacy row AND a modern row with `finding_id`; legacy dropped, modern used.
- `window_boundary_entry_at_exact_cutoff_uses_inclusive_start` — pin inclusivity policy (start-inclusive, end-exclusive is conventional).
- `window_with_no_qualifying_entries_returns_zero_count_no_panic`.

**Mystery-guest cleanup:** all `Provenance::External { /* ... */ }` placeholders in the implementation plan get replaced with explicit minimal constructors before TDD starts. Same for `AutoCalibrate(/* ... */)`.

**Plan-test sufficiency:** revised. Names describe precedence rules; Wontfix/Partial/multi-Human/time-window edges all covered.

---

### Task 8 — Channel attribution table

**Acceptance criteria**
- Columns: Channel, Total, TP, FP, Part, Wfix.
- No precision column (the design doc explicitly removes it for External/AutoCalib; consistency means no precision column at all in this table).
- Row order: Human, PostFix, External, AutoCalib (annotated "excluded from precision"), Unknown (annotated "legacy").
- Empty cells render as em-dash (`—`).
- Single dim `─` rule under header row only.
- Numeric columns right-aligned to value, not header.

**Tests beyond the plan sketch**
- `channel_attribution_postfix_row_renders_em_dash_for_fp_part_wfix` — by construction PostFix has only TPs.
- `channel_attribution_external_excluded_annotation_present` — assert the substring "excluded from precision" appears on the AutoCalib row (and verify no such annotation on the Human row).
- `channel_attribution_unknown_row_appears_only_when_legacy_entries_exist` — fresh corpus with no legacy entries: assert the Unknown row is absent (avoid noise).
- `channel_attribution_zero_corpus_renders_header_only` — empty feedback: header + rule, no data rows. Don't emit "no data" prose; the empty table is the message.
- `channel_attribution_aligns_thousands_separators` — 2,002 vs 27 must right-align on the comma.

**Plan-test sufficiency:** the rule-count test (`assert!(rule_count >= 1 && rule_count <= 6)`) is too loose — pin to exactly one. Otherwise solid.

---

### Task 9 — External corpus block

**Acceptance criteria**
- `compute_external_overlap(entries, quorum_verdicts)` returns `ExternalOverlap { per_agent: HashMap<String, AgentOverlap> }`.
- `AgentOverlap { findings, agreement_rate, disagreement_rate, corpus_contribution }`.
- Agreement = External and quorum both flagged the finding **and** assigned the same verdict family (TP/FP).
- Disagreement = both flagged the same finding with opposing verdicts.
- Findings only flagged by External (not by quorum) count toward `corpus_contribution` but not toward agreement/disagreement.
- Agents with `findings < 5` render `—` for agreement/disagreement (low-sample suppression).

**Tests beyond the plan sketch**
- `external_overlap_agent_with_only_one_finding_renders_low_sample` — three calls: 1, 4, 5 — the first two suppress, the last shows.
- `external_overlap_per_agent_normalization` — agent name "PAL" and "pal" collapse to one row (matches the External-ingestion lowercase normalization).
- `external_overlap_corpus_contribution_counts_unique_findings_not_entries` — same finding flagged twice by `pal` counts as 1 contribution.
- `external_overlap_agreement_partial_vs_tp_classified_as_disagreement` — pin the verdict-family policy: Partial vs TP is disagreement, not agreement.

**Plan-test sufficiency:** plan has one test. Missing low-sample, name normalization, contribution dedup, and partial-vs-tp classification — all observable in the rendered table.

---

### Task 10 — Headline trend rendering

**Acceptance criteria**
- Renders one line: `oldest% → ... → current% [lo-hi] (n=N) ↑|↓|→`.
- Wilson 95% CI shown only on the most recent window when `n ≥ 30`.
- Windows with `n < 30` rendered as `n<30` (no percentage).
- Capture rate appears as a sub-line.
- When `headline_trend_uses_finding_id == false`, a banner reads "entry-level pending finding-id rollout" and CI is suppressed (don't imply per-finding precision when we don't have it).
- Direction arrow compares first-window vs last-window; equal → `→`.

**Tests beyond the plan sketch**
- `headline_trend_n_exactly_30_renders_ci` — boundary; CI must appear at `n=30`.
- `headline_trend_n_eq_29_renders_n_too_low` — paired boundary.
- `headline_trend_all_windows_below_threshold_emits_summary_marker` — every window n<30: don't crash, render a "low data" line.
- `headline_trend_single_window_only_no_arrow` — only the current window has data: no `→` arrow (no comparison anchor).
- `headline_trend_capture_zero_total_findings_renders_dash` — `total=0`: capture line shows "—" not "NaN%".
- `headline_trend_legacy_mode_does_not_render_ci` — `headline_trend_uses_finding_id=false`: assert absence of `[lo-hi]` substring.

**Plan-test sufficiency:** the n=30 boundary is the single most important untested case (the plan tests n=8 vs n=32 but skips the threshold itself). Add it.

---

### Task 11 — Section label normalization

**Acceptance criteria**
- All `(7d)` → `(last 7 days)` in section headers.
- "Rolling 50 reviews" → "Rolling windows (50 reviews each)".
- Sparkline legend appears at first occurrence in the rendered output.

**Tests beyond the plan sketch**
- `section_labels_no_remaining_7d_substring` — grep the rendered output for `(7d)`; assert zero matches. Catches partial migrations.
- `sparkline_legend_renders_only_once` — count occurrences in `format_human` output.

**Plan-test sufficiency:** mechanical — the grep test is the only one that matters.

---

### Task 12 — `--full` flag

**Acceptance criteria**
- Default `quorum stats` omits "By caller" and "Rolling windows" sections.
- "By repo" remains in default output.
- `quorum stats --full` shows all dimensions.
- Default output stays under ~30 lines (per design doc budget).

**Tests beyond the plan sketch**
- `default_stats_output_under_30_lines` — count lines, assert `<= 30`. Pins the budget.
- `full_stats_output_does_not_include_join_health_unless_flag_passed` — orthogonality of `--full` and `--join-health`.
- `full_stats_with_no_caller_data_renders_empty_section_or_omits` — pin the "no data" policy for the gated sections.

**Plan-test sufficiency:** sketches are fine. The 30-line budget assertion is the missing high-signal test.

---

### Task 13 — DESIGN.md updates

**Acceptance criteria**
- §4.x and §12.x exist and contain the table-rule and trend-interpretation conventions.
- Examples match what `format_human` actually emits (test by snapshot — see Task 14).

**Tests beyond the plan sketch**
- N/A directly, but Task 14's snapshot test should be linkable from the design doc as the source of truth.

---

### Task 14 — Final integration

**Acceptance criteria**
- `format_human_core` emits sections in this order: Feedback Health → Channel attribution → Headline trend → Activity → Spend → External corpus → By repo → (gated) By caller → (gated) Rolling.
- All Phase 0 schema changes flow visibly through to the rendering: `--join-health`, headline CI, channel attribution all reflect synthetic test corpus correctly.

**Tests (revised — structural, not full-output snapshots)**

Antipattern review pushed back on full-output snapshots: output is being actively redesigned; snapshots get blind-updated during normal copy iteration. Use structural assertions instead:

- `format_human_default_section_order` — extract section headers via a helper `section_headers(&out)`; assert `vec!["Feedback Health", "Channel attribution", "Headline trend", "Activity", "Spend", "External corpus", "By repo"]`.
- `format_human_full_adds_caller_and_rolling_in_order` — same helper, `--full=true`, assert `"By caller"` and `"Rolling windows"` present at correct positions.
- `format_human_with_legacy_only_corpus_emits_banner` — only legacy entries, no `finding_id`s anywhere: assert banner string present, CI absent (substring assertions).
- `format_human_with_zero_feedback_does_not_panic` — totally empty feedback file: every section emits empty/dash form, no panics. Assert structural correctness without pinning copy.
- `format_human_compact_mode_unchanged` — compact mode (LLM-consumption format) is out-of-scope for this redesign; assert specific known substrings remain (`"feedback:"`, `"precision:"`, etc.) — NOT a full snapshot.

**Why no full snapshots:** copy iteration during Phase A would generate "update snapshot" commits indistinguishable from real regressions. Structural tests survive copy edits. If a regression net is wanted later, frozen snapshots can be added once design stabilizes.

**Plan-test sufficiency:** structural section-order test is the load-bearing wiring guarantee for Tasks 6 → 10 → 14.

---

## Cases the plan missed entirely (top 5)

1. **`skip_serializing_if = "Option::is_none"` is not asserted anywhere.** Without it, every legacy row rewritten by the daemon gets `"finding_id": null` injected, bloating ~1,470 rows by ~20 bytes each. Add to Task 1.

2. **Multi-Human-verdict conflict resolution.** Two Human entries on the same finding (FP, then TP after correction) — which wins? The plan doesn't pin a policy. Latest-by-timestamp is the obvious choice but must be tested *and* documented.

3. **Wontfix + PostFix interaction.** A Wontfix Human + a PostFix TP on the same finding must resolve to Wontfix (suppression), not TP. The plan's tests cover Human FP + PostFix TP and Human TP + PostFix TP, but not Wontfix — and Wontfix is the case where getting it wrong silently *inflates* reported precision.

4. **n=30 sample-size boundary.** The plan tests n=8 (below) and n=32 (above) but never n=30 itself. The threshold is the single most user-visible decision in the headline trend — it must be tested at the boundary in both directions (n=29 → "n<30", n=30 → CI shown).

5. **Empty-corpus rendering.** No test in the plan exercises `quorum stats` against an empty `feedback.jsonl` and `reviews.jsonl`. Every section must degrade gracefully (em-dash, "no data," or omission) without panic or NaN.

---

## Tasks where the existing test sketch is insufficient

- **Task 1** — missing the `null`-on-disk regression test (highest-priority migration safety).
- **Task 6** — the tautological assertion (`x || !x`) is a no-op; replace with the `≥85% linkage → flag=true` threshold test.
- **Task 7** — covers basic precedence but misses Wontfix, multi-Human conflict, and time-window inclusivity. These are the high-impact precedence edges.
- **Task 8** — the rule-count test (`>=1 && <=6`) is too loose; pin to exactly 1.
- **Task 10** — n=30 boundary is the most important headline-trend test and is absent.
- **Task 14** — needs golden snapshots; without them, the cross-task wiring is untested end-to-end.

---

## Suggested test infrastructure additions

- A `feedback_builder` fixture in `src/feedback.rs::tests` for ergonomic construction (`FeedbackBuilder::new().finding_id("A").human().tp().at(timestamp).build()`).
- A `corpus_builder` fixture for `(reviews, feedback)` pairs with controllable linkage rates.
- Golden snapshot directory at `tests/snapshots/stats/` checked into the repo; refresh procedure documented in DESIGN.md §12.x.
- A `min_sample_threshold()` constant (currently `MIN_SAMPLE=5` for stats dimensions, `n>=30` for Wilson) — define once, reference everywhere, test the boundaries against the constant not magic numbers.
