# Calibrator Threshold Stability (Phase 1, #458) — Implementation Plan (rev 2, post-review)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the calibrator's operating point trustworthy — first by *measuring* whether the observed suppress/boost value-flap (0.317↔0.742) is behavioral or cosmetic, then (only if behavioral) holding the deployed threshold when it is still operationally safe under the new model.

**Architecture:** Observability-first. `learn_logistic` already anchors each threshold to an operating point (suppress = 99% TP-recall percentile of OOF preds, calibrate.rs:2484-2498), and the report already prints AP/lift/FP-recall/suppress/boost (main.rs:4021-4032). Rev 1 tried to freeze the raw *value* via a bootstrap CI; a two-model review (gpt-5.5 + Fable) showed that is mis-targeted — the value moves because the logistic P(FP) score axis is non-stationary across retrains, so holding a prior value against new scores deploys a *third, unintended* operating point. Rev 2: (A) report candidate-vs-deployed + an instability band so we can see what the churn actually does; (B) if the band shows real behavioral swings, add a **safety-gated hold** — keep the prior threshold only if it still meets the 99%-TP-recall constraint on the *current* model's OOF (else adopt). No bootstrap.

**Tech Stack:** Rust, existing `crate::metrics::fp_recall_at_tp_recall`.

**Scope note:** Interim bridge — Phase 4 (#457) replaces fitted thresholds with policy config. No deletions, no review-time decision-path changes.

**Review outcome (gpt-5.5 via codex-cli + Fable 5), folded in below:**
1. **Confirmed bug (gpt-5.5):** `run_calibrate` builds a FRESH model from feedback (main.rs:3946) whose `logistic_model` is `None`; the persisted load at main.rs:2137 is a *different* path. Capturing prior from that fresh model gives nothing → any guard is inert. **Must explicitly load `calibrator_model.toml` inside `run_calibrate` before learning.**
2. **Freeze behavior, not value (both):** replace value-CI bracketing with a current-corpus safety gate.
3. **Bootstrap dropped (both):** high-variance/discrete on a 1%/5% tail; and Phase 4's tripwire wants a CI on the FP-recall *distribution*, not a tail order-statistic — so the "reused later" justification was wrong.
4. **Determinism DoD narrowed (both):** `learn_logistic` is already deterministic (group_k_fold first-appearance, fixed lambda order, no RNG). Byte-identical *model file* is impossible (`computed_at`, HashMap TOML order). DoD is: **same ordered corpus → identical deployed thresholds**, and canonicalize joined-sample order before CV.
5. **Two-corpus regression (both):** the flap test must train two real adjacent corpora, not construct a bracket.

---

## File structure

- `src/main.rs` — `run_calibrate`: load prior `calibrator_model.toml` explicitly; canonicalize joined-sample order before learning; augment the report with candidate/deployed/instability; (Task 5, conditional) apply the safety gate.
- `src/calibrate.rs` — canonical sample sort helper; `threshold_safe_under` predicate (does a given threshold meet the operating constraint on an OOF array); (Task 5) `hold_or_adopt_safe`.
- `src/metrics.rs` — no change (FP-recall already exists).
- Tests: in-file `#[cfg(test)] mod tests` in `calibrate.rs`.

**Execution-time verification (do first, with rust-expert):** trace how `run_calibrate` obtains the model it writes (fresh at main.rs:3946 vs loaded at 2137) and confirm the prior thresholds must come from an explicit `CalibratorModel::load_from(model_path)` inside `run_calibrate`. Nail the suppress/boost inequality direction (suppress-when-`score < threshold` vs `>`) against calibrate.rs:744-768 before writing the safety predicate.

---

## Task 1: Canonicalize joined-sample order before CV (determinism prerequisite)

**Files:** Modify `src/calibrate.rs` (sort `Vec<JoinedSample>` deterministically at the top of `learn_logistic`, or in `run_calibrate` before the call); Test: `calibrate.rs` tests.

- [ ] **Step 1: Failing test** — build a `Vec<JoinedSample>`, train, capture thresholds; shuffle the input vec, train again; assert identical suppress/boost.

```rust
#[test]
fn thresholds_are_order_invariant() {
    let a = build_sample_corpus();           // existing test builder
    let mut b = a.clone();
    b.reverse();
    let ra = learn_logistic(&a, 5).unwrap();
    let rb = learn_logistic(&b, 5).unwrap();
    assert_eq!(ra.suppress_threshold, rb.suppress_threshold);
    assert_eq!(ra.boost_threshold, rb.boost_threshold);
}
```

- [ ] **Step 2: Run, verify it FAILS** (fold assignment is first-appearance based, so order changes folds).
- [ ] **Step 3: Implement** a stable sort by a canonical key (e.g. `(finding_id, file_path, title, score)`) before fold assignment. Document the key.
- [ ] **Step 4: Run, verify PASS.**
- [ ] **Step 5: Commit** — `test(calibrate): make thresholds order-invariant (#458)`.

---

## Task 2: Report candidate vs deployed + instability band (observability — the base)

**Files:** Modify `src/main.rs` `run_calibrate` `Some(result)` arm (4021-4050).

Rationale: this is the always-ship part. It costs almost nothing (FP-recall already printed) and produces the data that decides whether Task 5 is even warranted. "Deployed" == "candidate" until Task 5 exists.

- [ ] **Step 1:** Emit, per threshold: candidate value, deployed value (== candidate for now), FP-recall@99%TP (already at 4028), and — if a prior model loaded — the delta from prior. Add a `tracing::info!` structured line (threshold, prior, candidate, deployed, delta).

```rust
eprintln!(
    "  Suppress threshold: {:.4}  (candidate {:.4}; prior {}; d={})",
    dep_suppress, result.suppress_threshold,
    prior_suppress.map(|p| format!("{p:.4}")).unwrap_or_else(|| "none".into()),
    prior_suppress.map(|p| format!("{:+.4}", result.suppress_threshold - p)).unwrap_or_else(|| "n/a".into()),
);
```

- [ ] **Step 2:** Load the prior model explicitly (fixes review bug #1) to populate `prior_suppress`/`prior_boost`:

```rust
let prior = quorum::calibrator_model::CalibratorModel::load_from(&model_path.to_string_lossy());
let (prior_suppress, prior_boost) = prior
    .and_then(|m| m.logistic_model)
    .map(|l| (Some(l.suppress_threshold), Some(l.boost_threshold)))
    .unwrap_or((None, None));
```
(Confirm `model_path` is in scope here; it is used at main.rs:2137/4257.)

- [ ] **Step 3:** Build + smoke — `cargo run -- calibrate --dry-run`; confirm the new lines render and prior deltas appear on a second run.
- [ ] **Step 4: Commit** — `feat(calibrate): report candidate/deployed/prior-delta for thresholds (#458)`.

---

## Task 3: `threshold_safe_under` predicate (pure, for the gate)

**Files:** Modify `src/calibrate.rs`; Test: `calibrate.rs` tests.

```rust
/// True if using `threshold` as the suppress cutoff on `oof` (current model's
/// OOF predictions for the TP class) still keeps TP-recall >= `min_tp_recall`.
/// (Boost side: analogous with the FP class and the boost inequality.)
/// Direction of the inequality MUST match calibrate.rs:744-768 — verify first.
pub(crate) fn threshold_safe_under(oof_tp: &[f64], threshold: f64, min_tp_recall: f64) -> bool { /* ... */ }
```

- [ ] **Step 1: Failing tests** — a prior threshold that still clears 99% TP-recall on a fresh OOF array → true; one that now suppresses >1% of TPs → false; empty → false.
- [ ] **Step 2: Run, verify fail. Step 3: Implement (count fraction of TP OOF preds on the safe side of `threshold`). Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `feat(calibrate): threshold_safe_under operating-constraint predicate (#458)`.

---

## Task 4: DECISION GATE — is a hold even warranted?

**Not a code task.** After Tasks 1–2 land and you have run `calibrate` on the real corpus a few times (or replayed recent `calibrator_model.toml` history), inspect the reported instability:

- If candidate thresholds barely move once the corpus is order-canonical, **the flap was cosmetic → STOP. Phase 1 is done at Task 2** (observability). Skip Task 5; proceed to Phase 2 (#459).
- If candidates swing materially *and* the swing changes realized suppression on held-out findings, **proceed to Task 5.**

Record the finding (numbers) in the issue #458 thread either way. This gate is the whole point of observability-first: don't build the guard on an unconfirmed premise.

---

## Task 5 (CONDITIONAL): Safety-gated hold

**Only if Task 4 confirms behavioral swings.** Files: `src/calibrate.rs` (`hold_or_adopt_safe`), `src/main.rs` (wire in).

```rust
/// Keep the prior threshold only if it is still operationally safe under the
/// current model; otherwise adopt the candidate. Returns (deployed, held).
pub(crate) fn hold_or_adopt_safe(
    prior: Option<f64>, candidate: f64, oof_tp: &[f64], min_tp_recall: f64,
) -> (f64, bool) {
    match prior {
        Some(p) if threshold_safe_under(oof_tp, p, min_tp_recall) => (p, true),
        _ => (candidate, false),
    }
}
```

- [ ] **Step 1: Two-corpus regression (replaces rev-1 Task 5).** Build two real adjacent corpora A and B differing by ~1 sample that reproduce candidate ≈0.317 (A) then ≈0.742 (B). Train A, deploy 0.317. Train B; assert `hold_or_adopt_safe(Some(0.317), 0.742, oof_tp_B, 0.99)` **holds 0.317 IFF 0.317 still clears 99% TP-recall on B's OOF**, and adopts otherwise. This tests real behavior, not a constructed bracket.
- [ ] **Step 2–4:** implement, wire into `run_calibrate` (deployed value flows into the emitted `LogisticModel` + the report's "deployed" column + a `held` flag), verify.
- [ ] **Step 5: Commit** — `feat(calibrate): safety-gated threshold hold (#458)`.

---

## Verification

- [ ] `cargo test --bin quorum` green; `cargo clippy --all-targets -- -D warnings` clean; `cargo build --release` ok.
- [ ] `cargo run -- calibrate --dry-run` twice on the **same ordered corpus** → identical deployed thresholds (DoD, narrowed per review). Report shows candidate/deployed/prior-delta/FP-recall.

## DoD (revised)

1. Thresholds are order-invariant and identical across reruns on the same ordered corpus (Task 1).
2. The calibrate report surfaces candidate, deployed, prior-delta, and FP-recall@99%TP per threshold (Task 2).
3. A documented decision (Task 4) on whether the flap is behavioral, recorded in #458.
4. If behavioral: prior thresholds are held only when still safe under the current model, proven by a two-corpus regression (Task 5).

## Self-review notes

- The confirmed prior-capture bug (review #1) is fixed in Task 2 Step 2 (explicit load), so it lands even in the observability-only path.
- Bootstrap/CI removed entirely (review #3). No `LogisticResult` field changes needed now.
- Task 4 is the ponytail guard against building a mechanism for a cosmetic problem.
