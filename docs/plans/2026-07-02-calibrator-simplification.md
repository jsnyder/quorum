# Calibrator: Replace the Learned Model with a Transparent Lookup

**Date:** 2026-07-02
**Author:** Fable 5 (lead architecture review → spec)
**Related issues:** #437 (MERGED — finding_id linkage forward), #439 (OPEN — legacy finding_id backfill), #389 (OPEN — consolidate three join fns)
**Basis:** Design review 2026-07-02 — three parallel decision paths, threshold flapping 0.317↔0.742, 37% join yield / 63% verdict loss, TP-enriched biased corpus, ~1000 LOC dead code. Numbers are run-observed unless labeled otherwise.

## North star (the outcome, not the tidiness)

What the maintainer has wanted from calibration and not gotten: **suppress real false positives without eating true positives — predictably, and in a way they can inspect, reason about, and hand-correct.** Every path below is judged against that, not against code cleanliness. Transparency is itself a feature that's been denied: a model the maintainer can open, understand, and override beats a black box that scores marginally higher on a metric they can't feel. "Predictably" matters as much as "accurately" — a system whose deployed behavior flips between retrains is untrustworthy even when its average is fine.

## Problem

The calibrator "does things but is problematic," and **several minor/moderate revisions have already been spent without delivering.** That track record is evidence, not noise. Three verified failure modes explain why:

1. **The operating point flaps.** The logistic model's suppress/boost thresholds are recomputed from OOF percentiles every retrain. On a corpus differing by ~1 joined sample, suppress moved 0.317 → 0.742 and boost 0.162 → 0.048 (run-observed). This is the "stuck" feeling: retraining silently flips behavior.

2. **The learned signal is thrown away at the door.** The trace↔feedback join is a six-tier text/path cascade keyed on `(finding_title, file_path)` strings. Run-observed: 1,675 / 4,504 eligible verdicts join (37%); 742 dropped ambiguous, 1,163 below-threshold, 924 unmatched. FP verdicts join ~30% worse than TP (25.9% FP eligible → 19.5% FP joined), so the model trains on a TP-enriched, benchmark-enriched slice and meets human triage at review time.

3. **The "model" is 4 numbers in a costume.** Run-observed z-scored coefficients: `file_fp_rate` 0.20, `min_word_lor` 0.14, `max_word_lor` 0.11, `category_fp_rate` 0.05 carry it; 10 of 22 features are selected but the three `log1p_*_weight` features sit at 0.003–0.007 (dead), and the other 12 never make the cut. And it fails the one metric that matters at a safe operating point: **FP-recall@99%TP is 2.7–6.3%** (run-observed) — it can suppress almost no FPs without eating TPs. A model that can only be evaluated by a metric it fails is not worth iterating.

**Prior art (do NOT re-propose).** #437 landed `finding_id` (ULID) forward: FindingMeta write path at review completion, `resolve_finding_id()` auto-link (Jaccard+substring, 0.6), feedback auto-resolves finding_id, CLI `--finding-id`, MCP `findingId`, `Finding.id` in `--json`. The ULID exists, is emitted, and is echoable today; the 3.7%-populated rate is only because it fills forward on a mostly-legacy corpus.

**Keystone gap #437 did NOT close:** `CalibratorTraceEntry` (calibrator_trace.rs:63) has `run_id` in `TraceProvenance` but **no `finding_id`**, and the calibrator join never keys on an id. #437 linked feedback→review; nobody put finding_id into the trace or its join.

---

## Decision: Refactor vs Replace → **REPLACE**

Two honest options:

- **Path A — Refactor:** keep the logistic pipeline (GroupKFold CV, univariate screen, consensus selection, lambda grid, OOF percentile thresholds, composite fallback), stabilize thresholds, shrink features, measure bias. Lovingly simplify what exists.
- **Path B — Replace:** delete the learned model + CV/logistic/composite machinery and install a **transparent beta-smoothed lookup** — per-file and per-category FP rates shrunk toward a global prior, plus the existing word-LOR title prior — producing a suppress score, with **policy thresholds** (a chosen precision/operating point, not a fit-from-percentile).

**I recommend Path B, plainly.** Not as a preference — as the read the evidence forces:

1. **Path A is not untried; it's a failure record.** The maintainer has already spent several revisions here and the system still doesn't deliver. Continuing to refactor the same architecture is choosing the strategy that has already not worked.
2. **The threshold churn is intrinsic to the approach, not a bug in it.** Fitting a suppress cutoff from OOF percentiles over ~330 FPs on a biased sample will always be knife-edge. You can't refactor your way out of "the operating point is a fragile statistic." Path B sets the operating point by *policy*, so the churn disappears by construction.
3. **The model is already a lookup.** Its live signal is `file_fp_rate` + `category_fp_rate` + word-LOR — exactly the lookup Path B builds. The CV/consensus/lambda machinery is ceremony wrapped around 4 numbers. Path B keeps the 4 numbers and deletes the ceremony.
4. **It fails its own yardstick.** FP-recall@99%TP 2.7–6.3% means the fancy model provides essentially nothing at a safe operating point. There is no marginal accuracy to protect.
5. **Transparency is the denied feature.** With a lookup, the maintainer can open the rate table, see "`error handling` has FP-rate 0.33," disagree, and correct it. That's the trustable-and-reasonable-about property the north star demands, which a logistic coefficient vector cannot offer.

**Tripwire (prove it or it's gone — default is gone).** After the finding_id join lands and the corpus is clean (Phases 1–3), run the lookup and a *minimal* (≤5-feature) logistic in shadow for one evaluation window on the id-joined corpus. Keep the logistic **only if** it beats the lookup on FP-recall@99%TP by a margin that is both statistically real (outside the bootstrap CI) **and** operationally meaningful (≥10 absolute points — roughly doubling today's recall). Otherwise it is deleted. The burden of proof is on the fancy model; ties and marginal wins go to the transparent lookup.

**Shared no-regret prefix.** The joinable-data work and operating-point stability are valuable under *both* paths — the lookup needs a clean, unbiased, id-keyed corpus every bit as much as the logistic does, and a stable operating point is the north star regardless. So Phases 1–3 come first no matter what, and the fork only resolves once we can actually *measure* whether the fancy model earns its keep. That shared-prefix framing is the real answer: fix the data and the stability first (no-regret), then let evidence — not today's argument — retire the logistic.

---

## Scope

Five ordered, independently-shippable phases. Phases 1–3 are no-regret (hold under refactor or replace). Phases 4–5 execute the replacement. Out of scope: the precedent-weight semantics (`verdict_weight` / FpKind decay — #123's domain), context-injection calibration, and any review-prompt change.

---

## Phase 1 — Operating-point stability (interim bridge)

**Status: NEW.** No existing issue. Smallest diff, immediate relief.

**Why first:** The threshold flap is the live "stuck" symptom and a tiny change. It buys predictable behavior *while* the replacement is built. Under Path B it's ultimately superseded by policy thresholds — but it's cheap insurance for the interim, and the FP-recall instrumentation it adds is permanent.

**Design:**
- Replace "recompute suppress/boost from OOF percentile every retrain" with a **move-only-on-confidence** guard: bootstrap the OOF predictions for a CI on the candidate threshold; keep the deployed value when it's inside the CI, adopt only when outside, log the decision (n, direction, magnitude).
- Add **FP-recall@99%TP to the calibrate report** as a first-class number (it's the metric the north star cares about and the one Path B's tripwire uses). Surface it for the current model now.

**Definition of done:**
- Two identical-corpus `calibrate` runs → byte-identical deployed thresholds.
- Report prints, per threshold: candidate, CI, held-from-prior flag, and FP-recall@99%TP.

**Test plan:** held-vs-adopted by CI bracket; deterministic bootstrap (reuse `deterministic_permutation`); regression against the 0.317/0.742 corpora holding the deployed value; FP-recall surfaced correctly.

**Risk:** Low — adds a guard + a metric, deletes nothing. Over-stickiness bounded by the out-of-CI escape.

---

## Phase 2 — finding_id into traces + tier-0 id join (keystone, no-regret)

**Status: NEW.** Builds on #437. Valuable under both paths — the lookup needs an unbiased joinable corpus as much as any model.

**Design:**
- Add `finding_id: Option<String>` to `CalibratorTraceEntry` (additive serde `default` + `skip_serializing_if`, matching the existing `file_path`/`in_diff` pattern). Populate from the `Finding.id` #437 already sets, in the trace write path (`make_trace_entry`, in scope there).
- Add a **tier-0 exact-id join**: when both sides carry `finding_id`, join is a `HashMap<Ulid, weights>` equality lookup — no normalization, no Jaccard, no ambiguity. Tier-0 beats every text tier.
- Wire tier-0 into the ONE join logistic/lookup training consumes (`extract_joined_samples`); leave the other two joins for Phase 3 (don't triplicate id logic).
- **Honest expectation:** this is go-forward. The 17k historical traces have no id; tier-0 yield starts near zero and grows. The text cascade stays as fallback and *drains*. Report tier-0 vs cascade counts so the drain is visible.

**Definition of done:** new traces carry finding_id; an id-equal pair joins tier-0 despite differing titles; report shows id vs text counts; legacy no-id traces join exactly as before (zero historical regression).

**Test plan:** serde round-trip + pre-bump None; tier-0 precedence; id-on-one-side falls through; **trace id == `--json` Finding.id for the same run** (single-source guard); duplicate id caught, not silently dropped.

**Risk:** Medium — silent id mismatch mis-joins tier-0. Mitigation: single-source the id, exact equality only (never fuzzy-match ids), and the `--json` equality test.

---

## Phase 3 — Consolidate the join into one canonical implementation (no-regret)

**Status: COVERED-BY #389 (this scopes and supersedes it).** #389 asks to merge `join_feedback_and_traces_with_options`, `extract_join_features`, `extract_joined_samples` with property tests.

**Why here:** Both training paths and the lookup read one join; the run-observed 1,692-vs-1,675 sample discrepancy (two joins disagreeing on the same run) must die before we compare models. Consolidate *toward the Phase-2 shape* (tier-0 id → raw exact → normalized exact → cascade), not toward the legacy tangle.

**Design:** one canonical join; the other two become adapters or are deleted. Keep the fuzzy/deep-path/suffix/title-only tiers intact here (they're deleted in Phase 5) so this PR is behavior-preserving except the count reconciliation. Add #389's property tests.

**Definition of done:** exactly one join; training corpus and calibrate report agree on sample/FP counts; property suite green.

**Test plan:** golden corpus reproduction; order-independence / tier-attribution / idempotence properties; three-old-vs-one-new diff harness on the real corpus reconciling 1,692↔1,675.

**Risk:** Medium (largest refactor). Mitigation: land after Phase 2 so it's a simplification; keep cascade tiers so it's behavior-preserving.

---

## Phase 4 — Build the transparent lookup + policy thresholds; shadow-eval + resolve the tripwire

**Status: NEW (the replacement).** This is where Path B lands and the fork resolves.

**Design:**
- **The model is a lookup, not a fit.** Per-file and per-category FP rates, each **beta-smoothed toward the global FP rate** (shrinkage by support count — a file/category with 2 observations barely moves off the prior; one with 200 dominates), combined with the existing word-LOR title prior into a single suppress score in [0,1]. These rate maps already exist and already serialize in `calibrator_model.toml`; this phase makes them *the* model instead of logistic inputs.
- **Category-key normalization is intrinsic and lands here** (run-observed: `race condition`/`race_condition`/`race-condition` are three keys; `error handling` 0.33 vs `error-handling` 0.19). Normalize before building rate maps — this is free accuracy and a transparency win (one row per concept the maintainer can inspect).
- **Policy thresholds, not fitted.** The suppress/boost cutoffs are chosen against an explicit precision target + FP-recall floor and written as config the maintainer can read and override — no OOF percentile, no per-retrain drift. This is what structurally kills the churn.
- **Hand-correctability:** the rate table is inspectable and overridable (a maintainer-pinned rate for a category/file survives recompute). State this as a first-class capability.
- **Selection-bias table** (join-rate × verdict × provenance) lands in the report here — it's how we keep the corpus honest and how we down-weight/segregate the `quorum-benchmark` slice (run-observed 1,377 rows, 86% TP, 30% of labels).
- **Resolve the tripwire:** run the lookup and a minimal ≤5-feature logistic in shadow on the clean id-joined corpus for one window; report FP-recall@99%TP + bootstrap CI for both, and the per-file beta-smoothed baseline. Record which wins by the tripwire rule.

**Definition of done:**
- Lookup produces suppress/boost decisions; thresholds are policy config, stable across identical-corpus recomputes.
- Rate maps built on normalized keys; a pinned override survives recompute.
- Report prints: lookup vs minimal-logistic vs baseline FP-recall@99%TP with CIs, and the join-bias table.
- Tripwire outcome recorded (default: logistic does not clear the bar).

**Test plan:** beta-shrinkage moves low-support rates toward the prior and high-support toward the sample; three race-condition spellings collapse to one key/rate; policy thresholds don't move on identical-corpus recompute; pinned override persists; bias table on synthetic disparity; tripwire arithmetic (margin + CI) on synthetic model pairs.

**Risk:** Medium. The lookup could underperform on some category with sparse data — mitigated by shrinkage (sparse → global prior, the safe default) and by the fact that the operating point is now honest and inspectable rather than optimistic.

---

## Phase 5 — Rip out the logistic / CV / composite / compute_thresholds machinery

**Status: NEW deletion.** Executes the replacement once Phase 4 resolves the tripwire (default: logistic gone).

**Design — delete list, ranked by lines × risk-reduced:**
1. **Logistic pipeline:** `learn_logistic`, GroupKFold (`group_k_fold`), `univariate_screen`, consensus selection, lambda grid, `logistic.rs` fit/predict as used by the calibrator, `ExpandedFeatures` (22-dim vector), OOF percentile threshold selection, `LogisticModel` from `calibrator_model.rs`. Gone unless the tripwire kept it.
2. **Composite path:** `ScoreWeights`, `composite_score`, `learn_weights`, `grid_search_best`, `weights_stable`, `rescore_samples_with_model`, composite branch in `calibrate_core_decision` (calibrator.rs:597–668). Run-observed shadowed + hardcoded defaults.
3. **`compute_thresholds` + `calibrator_thresholds.toml` + `--suppress/boost-precision` flags** (threshold_config.rs, 23-byte file). Never produced output.
4. **Dead join tiers** (safe post-Phase-3): deep-path (0 joins), fuzzy same-file (45), suffix (16), title-only×2 (2+0). Keep tier-0/1/2.
5. **Legacy magic-number duplication:** collapse the soft-suppress/confirm rules (re-implemented at calibrator.rs:558/568 and 674/709) into one `fallback_decision()` for the no-model case.

**End state:** one transparent lookup + policy thresholds + one no-model fallback. Zero fitted thresholds, zero CV, zero composite, one join with three tiers.

**Definition of done:** grep-clean of logistic/composite/compute_thresholds (modulo a tripwire-kept minimal logistic, if any); `calibrate_core_decision` has one active path + one fallback; suite green; no tier-0/1/2 yield regression.

**Test plan:** decision-parity fixtures (lookup reproduces intended suppress/boost on a labeled set); `calibrate` no longer touches the thresholds toml; no-model fallback parity.

**Risk:** Low-to-medium. Deletion is safe because Phase 4 already proved the survivor on the clean corpus. Estimated ~1,200+ LOC src+tests removed.

---

## Migration

- New `Option<T>` fields (`CalibratorTraceEntry.finding_id`) are additive with serde `default`; pre-bump lines deserialize unchanged. No trace-file rewrite.
- `calibrator_thresholds.toml` (Phase 5): ignored then removable; document as safe to delete.
- The deployed logistic `calibrator_model.toml` stays loadable through Phase 4; Phase 4 writes the lookup model (rate maps are already in the file — this mostly stops writing the `[logistic_model]` block and starts treating rate maps as primary). Phase 5 removes the logistic block entirely.
- **#439 (legacy finding_id backfill) is intentionally OFF the critical path** — see override note.

## Risks (global)

- **Silent id mismatch** (Phase 2) — single-source the id, exact equality, `--json` parity test.
- **Sparse-category rates** (Phase 4) — beta shrinkage defaults them to the global prior (safe).
- **Tripwire mis-called** — require both CI-separation and a ≥10-point margin; default to the lookup on ambiguity.

## Definition of done (overall)

One transparent, inspectable, hand-correctable lookup model; policy thresholds stable across identical-corpus recomputes; one id-keyed join with a draining text fallback; a standing join-bias table; FP-recall@99%TP reported for the deployed model and its baseline. The logistic/CV/composite machinery is gone (or explicitly kept only because it cleared the tripwire). The north star — predictable, trustable FP suppression the maintainer can reason about — is structurally achievable rather than fitted-and-hoped.

## Files touched (by phase)

- **P1:** `src/calibrate.rs` (threshold selection), `src/metrics.rs` (FP-recall), `src/main.rs` (report).
- **P2:** `src/calibrator_trace.rs` (field), `src/calibrator.rs` (`make_trace_entry`), `src/calibrate.rs` (`extract_joined_samples` tier-0), `src/main.rs` (report).
- **P3:** `src/calibrate.rs` (unify joins), tests.
- **P4:** `src/calibrate.rs` (lookup + shrinkage + key normalization + bias table + shadow-eval), `src/calibrator.rs` (decision uses lookup score), `src/calibrator_model.rs` (rate maps primary), `src/main.rs`.
- **P5:** `src/calibrate.rs`, `src/calibrator.rs`, `src/calibrator_model.rs`, `src/threshold_config.rs`, `src/logistic.rs`, `src/main.rs` (deletions).

## Out of scope

- FpKind / `verdict_weight` semantic decay (#123).
- Context-injection calibration / `context_misleading`.
- Review-prompt / few-shot changes.
- Retroactively healing the historical 63% join loss — the id join is go-forward; the old corpus drains, it doesn't heal.

---

## Where I overrode the proposed ordering

The originally-floated incremental order (freeze → finding_id → consolidate → delete → refit) treated the logistic architecture as the thing to preserve. I don't. Given the maintainer's stated track record — "several revisions, still doesn't deliver" — **the verdict is REPLACE, not refactor.** Concretely:

1. **The refit phase becomes a replacement.** Instead of shrinking the logistic to ~5 features (Path A's Phase 5), Phases 4–5 delete the learned model and install a transparent beta-smoothed lookup with policy thresholds. Incremental has been tried and structurally can't reach the north star: OOF-percentile thresholds are intrinsically churny, the joinable corpus is biased, and the model already collapses to 4 rate-based numbers while failing FP-recall@99%TP.
2. **No-regret prefix preserved and reordered up front.** finding_id-in-traces, join consolidation, and threshold stability come first because they're valuable under any outcome and let the tripwire be decided on *evidence over a clean corpus*, not on today's argument.
3. **#439 pulled off the critical path** — it backfills legacy *feedback*; historical *traces* can't get an id, so it barely helps the calibrator join. Ship it in parallel, later, for precision/dedup value only.
4. **Category-key normalization is intrinsic to the lookup** (Phase 4), not a tail-end tidy — under Path B the normalized rate table *is* the model.

---

## dev:start prompts

### Phase 1
> **Goal:** Stop the calibrator's suppress/boost thresholds from flapping between retrains. Replace per-retrain OOF-percentile selection with a bootstrap-CI move-only-on-confidence guard (keep deployed value inside CI, adopt only outside, log the decision). Add FP-recall@99%TP to the calibrate report as a first-class metric.
> **Constraints:** Touch only threshold selection + reporting; reuse the seeded permutation helper; no deletions; no new decision path.
> **DoD:** Identical-corpus runs → byte-identical thresholds; report prints candidate/CI/held-flag/FP-recall per threshold.
> **Test plan:** held-vs-adopted by CI bracket; deterministic bootstrap; regression vs the 0.317/0.742 corpora; FP-recall surfaced.
> **Out of scope:** Model replacement, join changes.

### Phase 2
> **Goal:** Add `finding_id: Option<String>` to `CalibratorTraceEntry`, populate from `Finding.id` (set by #437) in the trace write path, and add a tier-0 exact-id join to `extract_joined_samples` that beats all text tiers. Report join-by-id vs join-by-text counts.
> **Constraints:** Additive serde-default field; wire tier-0 into ONLY the training join (Phase 3 unifies the rest); exact id equality only, never fuzzy; text cascade stays as fallback.
> **DoD:** New traces carry finding_id; id-equal pair joins despite differing titles; report shows id vs text; legacy no-id traces unchanged.
> **Test plan:** serde round-trip + pre-bump None; tier-0 precedence; id-on-one-side fallthrough; trace id == `--json` Finding.id for one run; duplicate-id caught not dropped.
> **Out of scope:** Consolidating joins, deleting tiers, legacy backfill.

### Phase 3 (extends #389)
> **Goal:** Consolidate `join_feedback_and_traces_with_options`, `extract_join_features`, `extract_joined_samples` into one canonical join shaped tier-0 (id) → raw exact → normalized exact → cascade. Reconcile the 1,692-vs-1,675 discrepancy. Add #389's property tests.
> **Constraints:** Behavior-preserving except the count reconciliation; keep fuzzy/deep-path/suffix/title-only tiers (deleted in Phase 5); one join read by training and report.
> **DoD:** Exactly one join; training and report counts agree; property suite green.
> **Test plan:** golden reproduction; order-independence/tier-attribution/idempotence; three-old-vs-one-new diff harness.
> **Out of scope:** Deleting tiers, model changes.

### Phase 4
> **Goal:** Build the replacement: a transparent suppress model = per-file + per-category FP rates beta-smoothed toward the global prior + the existing word-LOR title prior, with policy thresholds (precision target + FP-recall floor, written as inspectable/overridable config — no OOF percentile). Normalize category keys before building rate maps. Add a join-bias table (join-rate × verdict × provenance) and down-weight/segregate the `quorum-benchmark` corpus. Shadow-run the lookup vs a minimal ≤5-feature logistic vs the per-file baseline; report FP-recall@99%TP + bootstrap CI for each and record the tripwire outcome.
> **Constraints:** Thresholds are policy config, not fitted; rate maps are the model; a maintainer-pinned rate survives recompute; keep the logistic only if it beats the lookup on FP-recall@99%TP by ≥10 points AND outside CI — else default to the lookup.
> **DoD:** Lookup drives decisions; identical-corpus recompute → stable thresholds; normalized keys; pinned override persists; report prints the three-way FP-recall comparison + bias table; tripwire outcome recorded.
> **Test plan:** shrinkage behavior (sparse→prior, dense→sample); three race-condition spellings → one key; stable thresholds on recompute; pinned override; bias table on synthetic disparity; tripwire arithmetic.
> **Out of scope:** Deleting the old machinery (Phase 5).

### Phase 5
> **Goal:** Delete the logistic pipeline (`learn_logistic`, GroupKFold, univariate screen, consensus selection, lambda grid, `ExpandedFeatures`, `LogisticModel`, OOF percentile thresholds), the composite path (`ScoreWeights`/`composite_score`/`learn_weights`/`grid_search_best`/`weights_stable`/`rescore_samples_with_model`), `compute_thresholds` + `calibrator_thresholds.toml` + `--suppress/boost-precision`, the dead join tiers, and collapse the duplicated legacy rules into one `fallback_decision()`. Lookup + policy thresholds is the sole path.
> **Constraints:** Only after Phase 4's tripwire resolves; keep a minimal logistic only if it cleared the bar; decision parity on a labeled fixture set; no tier-0/1/2 yield regression.
> **DoD:** grep-clean of the deleted machinery; one active path + one no-model fallback; suite green.
> **Test plan:** decision-parity fixtures; `calibrate` no longer touches the thresholds toml; no-model fallback parity.
> **Out of scope:** New feature work on the lookup.

---

## Proposed issues (draft — maintainer to file; not covered by #437/#439/#389)

1. **Replace the learned calibrator with a transparent beta-smoothed lookup + policy thresholds**
   The logistic model collapses to ~4 rate-based features, fails FP-recall@99%TP (2.7–6.3%), and churns thresholds between retrains; incremental revisions haven't delivered. Replace it with an inspectable per-file/per-category shrunk-rate lookup and policy thresholds. (Phases 4–5.)

2. **Put finding_id into CalibratorTraceEntry and key the calibrator join on it**
   #437 linked feedback→review but the trace has no finding_id and the join is still a 37%-yield text cascade. Add the field + tier-0 id-equality join. Go-forward keystone under any model. (Phase 2.)

3. **Freeze the calibrator operating point (interim CI-overlap guard + FP-recall metric)**
   Suppress/boost recompute from OOF percentiles; observed 0.317↔0.742 swing on a ~1-sample delta. Add a bootstrap-CI move-only-on-confidence guard and surface FP-recall@99%TP. (Phase 1.)

4. **Delete the composite calibrator scoring path**
   Shadowed by the current model whenever one exists, weights are hardcoded defaults, its threshold source never fires. Remove it. (Phase 5.)

5. **Remove compute_thresholds and calibrator_thresholds.toml**
   The precision-target threshold path has never produced output (23-byte file); delete it, the file, and `--suppress/boost-precision`. (Phase 5.)

6. **Measure and correct calibrator join selection bias**
   FP verdicts join ~30% worse than TP (25.9%→19.5% share); the benchmark corpus is 86% TP and 30% of labels. Add a join-rate × verdict × provenance report table and down-weight/segregate the benchmark slice. (Phase 4.)

7. **Normalize category keys before building FP-rate maps**
   `race condition`/`race_condition`/`race-condition` and `error handling`/`error-handling` (0.33 vs 0.19) are separate keys. Normalize before rate-map construction — accuracy + transparency win. (Phase 4, intrinsic to the lookup.)
