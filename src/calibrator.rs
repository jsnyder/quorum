/// Calibrator: adjusts findings using feedback precedent.
/// For each finding, searches for similar past findings and their TP/FP verdicts.
/// FP precedent suppresses findings; TP precedent boosts confidence.

use crate::feedback::{FeedbackEntry, Verdict};
use crate::finding::{CalibratorAction, Finding, Severity};

#[derive(Debug, Clone)]
pub struct CalibrationResult {
    pub findings: Vec<Finding>,
    pub suppressed: usize,
    pub boosted: usize,
    pub traces: Vec<crate::calibrator_trace::CalibratorTraceEntry>,
}

pub struct CalibratorConfig {
    /// Minimum similarity for Jaccard fallback (0.0 - 1.0)
    pub similarity_threshold: f64,
    /// Minimum similarity for embedding-based matching (higher because BGE clusters tightly)
    pub embedding_similarity_threshold: f64,
    /// Whether to boost severity when strong TP precedent exists
    pub boost_tp: bool,
    /// Whether to include auto-calibrate feedback in precedent matching
    pub use_auto_feedback: bool,
    /// Number of `Verdict::ContextMisleading` confirmations after which a
    /// chunk's injection threshold is sealed at `f32::INFINITY` (fully
    /// suppressed). Each prior confirmation raises the threshold linearly
    /// from the global floor toward 1.0.
    pub inject_suppress_after: u32,
}

impl Default for CalibratorConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.5,
            embedding_similarity_threshold: 0.72,
            boost_tp: true,
            use_auto_feedback: true,
            inject_suppress_after: 3,
        }
    }
}

// Issue #97: cap on the global External-provenance contribution to any single
// finding's TP or FP weight accumulator. Prevents a misbehaving or compromised
// agent from flooding verdicts and dominating calibration.
//
// 1.4 ≈ 2 fresh External TPs (each at 0.7 before recency decay). This ties
// max External influence to roughly 1 Human TP (1.0) or 1 PostFix (1.5) —
// External is advisory precedent, not authoritative. Cap applies GLOBALLY
// across agents (one pool summing pal + third-opinion + ...), not per-agent.
pub(crate) const EXTERNAL_WEIGHT_CAP: f64 = 1.4;

/// Bucket an iterator of `(provenance, weight)` pairs into three accumulators
/// (auto / external / humanish), apply per-bucket caps, and return the final
/// capped sum. Used by both `calibrate` (Jaccard path) and
/// `calibrate_with_index` (embedding path) so that the cap scheme stays in
/// exactly one place — a future cap-value or bucket-membership change can't
/// accidentally diverge between the two paths.
fn accumulate_capped<'a>(
    weighted: impl IntoIterator<Item = (&'a crate::feedback::Provenance, f64)>,
) -> f64 {
    let mut auto = 0.0_f64;
    let mut external = 0.0_f64;
    let mut humanish = 0.0_f64;
    for (prov, w) in weighted {
        match prov {
            crate::feedback::Provenance::AutoCalibrate(_) => auto += w,
            crate::feedback::Provenance::External { .. } => external += w,
            _ => humanish += w,
        }
    }
    auto.min(1.0) + external.min(EXTERNAL_WEIGHT_CAP) + humanish
}

/// Compute the weight of a single feedback entry based on provenance and recency.
///
/// `now` is injected (not pulled from `Utc::now()` inline) so calibrator tests
/// can pin time deterministically and mutation testing can lock the half-life
/// constants — antipatterns review #123 Phase 3 flagged the inline `Utc::now()`
/// as wall-clock-coupled. Production call sites in `calibrate` /
/// `calibrate_with_index` capture `Utc::now()` once at function entry and pass
/// the same `now` to every `verdict_weight` call so all entries within a single
/// calibration share the same reference time.
fn verdict_weight(entry: &FeedbackEntry, now: chrono::DateTime<chrono::Utc>) -> f64 {
    let provenance_weight = match &entry.provenance {
        crate::feedback::Provenance::PostFix => 1.5,
        crate::feedback::Provenance::Human => 1.0,
        // External: verdict from another review agent (pal, third-opinion, etc.).
        // 0.7x sits between Human (1.0) and AutoCalibrate (0.5) — cross-model
        // precedent is more trustworthy than the old self-verification loop but
        // less than a human who can see full PR context. `confidence` field is
        // deliberately ignored in v1 (stored for future use).
        crate::feedback::Provenance::External { .. } => 0.7,
        crate::feedback::Provenance::AutoCalibrate(_) => 0.5,
        crate::feedback::Provenance::Unknown => 0.3,
    };

    // Per-kind recency time-constant (#123 Layer 1).
    //
    // Match is intentionally exhaustive on `FpKind` (no `_ => 120.0` catch-all
    // for FP variants) so a new variant added later forces a compile-time
    // review point — it can't silently inherit the default decay if it should
    // rot faster (e.g. a future `BugFixedUpstream` would want a faster τ).
    //
    // - TrustModelAssumption: 40d (3x faster decay) — these rot quickly when
    //   the trust model evolves.
    // - Hallucination, CompensatingControl, PatternOvergeneralization,
    //   OutOfScope, and `None` (pre-bump rows) all use 120d (~83d half-life).
    // - Non-FP verdicts (Tp/Partial/Wontfix/ContextMisleading): 120d. fp_kind
    //   is meaningless here regardless of value.
    use crate::feedback::{FpKind, Verdict};
    let recency_tau_days = match (&entry.verdict, &entry.fp_kind) {
        (Verdict::Fp, Some(FpKind::TrustModelAssumption)) => 40.0,
        (Verdict::Fp, Some(FpKind::Hallucination)) => 120.0,
        (Verdict::Fp, Some(FpKind::CompensatingControl { .. })) => 120.0,
        (Verdict::Fp, Some(FpKind::PatternOvergeneralization { .. })) => 120.0,
        (Verdict::Fp, Some(FpKind::OutOfScope { .. })) => 120.0,
        (Verdict::Fp, None) => 120.0,
        (Verdict::Tp | Verdict::Partial | Verdict::Wontfix | Verdict::ContextMisleading { .. }, _) => 120.0,
    };

    // Future-dated entries (clock skew, mis-set system clocks, manual edits)
    // would otherwise clamp to age=0 and receive maximum recency weight.
    // Use absolute age so a future-dated entry decays the same as one written
    // the same delta in the past, instead of being the most-trusted precedent.
    let age_days = (now - entry.timestamp).num_days().unsigned_abs() as f64;
    let recency_weight = (-age_days / recency_tau_days).exp();

    provenance_weight * recency_weight
}

/// Calibrate findings using feedback precedent.
pub fn calibrate(
    findings: Vec<Finding>,
    feedback: &[FeedbackEntry],
    config: &CalibratorConfig,
) -> CalibrationResult {
    // Ablation knob: bypass calibrator post-processing entirely. Honored by
    // BOTH `calibrate` and `calibrate_with_index` so the env switch behaves
    // consistently regardless of whether the embedding index is available.
    // (Quorum self-review 2026-05-01 caught the inconsistency.)
    if std::env::var("QUORUM_DISABLE_CALIBRATOR").is_ok() {
        return CalibrationResult { findings, suppressed: 0, boosted: 0, traces: vec![] };
    }

    // Capture `now` once so every verdict_weight invocation in this calibration
    // uses the same reference time (#123 Layer 1 — clock injection refactor).
    let now = chrono::Utc::now();

    // Filter precedent pool. Two filters layered:
    // 1. Auto-calibrate exclusion (existing) — gated by config.use_auto_feedback.
    // 2. OutOfScope FP exclusion (#123 Layer 1) — these represent "real defect,
    //    tracked elsewhere", NOT suppression signal. Including them as FP
    //    precedents would suppress legitimate findings whose follow-ups are
    //    just deferred to other PRs/issues.
    let filtered: Vec<&FeedbackEntry> = feedback
        .iter()
        .filter(|e| {
            // OutOfScope FPs always excluded from the precedent pool.
            if let (Verdict::Fp, Some(crate::feedback::FpKind::OutOfScope { .. })) =
                (&e.verdict, &e.fp_kind)
            {
                return false;
            }
            // Auto-calibrate exclusion (when not enabled).
            config.use_auto_feedback
                || !matches!(e.provenance, crate::feedback::Provenance::AutoCalibrate(_))
        })
        .collect();

    if filtered.is_empty() {
        return CalibrationResult {
            findings,
            suppressed: 0,
            boosted: 0,
            traces: vec![],
        };
    }

    let mut output = Vec::new();
    let mut suppressed = 0;
    let mut boosted = 0;
    let mut traces = Vec::new();

    for mut finding in findings {
        let input_severity = finding.severity.clone();

        // Find similar feedback entries, filtering out metric-incompatible
        // precedents (e.g. CC=5 FP vs CC=11 finding) so they don't pollute
        // fp_weight/tp_weight.
        let similar: Vec<&&FeedbackEntry> = filtered
            .iter()
            .filter(|e| finding_feedback_similarity(&finding, e) >= config.similarity_threshold)
            .filter(|e| precedent_metric_compatible(&finding.title, &e.finding_title))
            .collect();

        if similar.is_empty() {
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight: 0.0,
                fp_weight: 0.0,
                wontfix_weight: 0.0,
                full_suppress_weight: 0.0,
                soft_fp_weight: 0.0,
                matched_precedents: vec![],
                action: None,
                input_severity: input_severity.clone(),
                output_severity: finding.severity.clone(),
                severity_change_reason: None,
            });
            output.push(finding);
            continue;
        }

        // Provenance-bucketed, per-bucket-capped weights. See `accumulate_capped`.
        let tp_weight = accumulate_capped(
            similar
                .iter()
                .filter(|e| matches!(e.verdict, Verdict::Tp | Verdict::Partial))
                .map(|e| (&e.provenance, verdict_weight(e, now))),
        );
        let fp_weight = accumulate_capped(
            similar
                .iter()
                .filter(|e| e.verdict == Verdict::Fp)
                .map(|e| (&e.provenance, verdict_weight(e, now))),
        );

        // Wontfix weight — retained only for trace diagnostics. Wontfix no longer
        // contributes to soft or full suppression (see inertness rationale below).
        let mut wontfix_weight: f64 = 0.0;
        for e in similar.iter().filter(|e| e.verdict == Verdict::Wontfix) {
            wontfix_weight += verdict_weight(e, now);
        }
        // Wontfix is inert: pre-existing issues the user chose not to fix carry no
        // signal about finding validity. Excluded from both soft and full suppression.
        let soft_fp_weight = fp_weight;

        // Build precedent traces for this finding. Each trace records the actual
        // Jaccard similarity (recomputed cheaply here — the filter above also uses
        // it) so operators debugging suppression see the real precedent strength.
        let matched_precedents: Vec<crate::calibrator_trace::PrecedentTrace> = similar
            .iter()
            .map(|e| {
                let sim = finding_feedback_similarity(&finding, e);
                crate::calibrator_trace::PrecedentTrace {
                    finding_title: e.finding_title.clone(),
                    verdict: e.verdict.clone(),
                    similarity: sim,
                    weight: verdict_weight(e, now),
                    provenance: serde_json::to_string(&e.provenance).unwrap_or_default(),
                    file_path: e.file_path.clone(),
                }
            })
            .collect();

        // Annotate with precedent info
        for entry in &similar {
            finding.similar_precedent.push(format!(
                "{}: {} ({})",
                match entry.verdict {
                    Verdict::Tp => "TP",
                    Verdict::Fp => "FP",
                    Verdict::Partial => "Partial",
                    Verdict::Wontfix => "Wontfix",
                    Verdict::ContextMisleading { .. } => "ContextMisleading",
                },
                entry.finding_title,
                entry.reason
            ));
        }

        // Full suppress: FP weight only. Wontfix no longer contributes.
        let full_suppress_weight = fp_weight;
        if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight,
                fp_weight,
                wontfix_weight,
                full_suppress_weight,
                soft_fp_weight,
                matched_precedents,
                action: finding.calibrator_action.clone(),
                input_severity,
                output_severity: finding.severity.clone(),
                severity_change_reason: None,
            });
            suppressed += 1;
            continue; // don't add to output
        }

        // Soft suppress: FP weight only (wontfix is inert), or auto-only FP
        // This preserves the finding for human review while reducing noise
        // Two triggers: (a) strong FP dominates TP; (b) modest FP, ~zero TP.
        if (soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0)
            || (soft_fp_weight >= 0.5 && tp_weight < 0.1)
        {
            finding.severity = Severity::Info;
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            // Don't increment suppressed — finding stays in output at reduced severity
        }

        // Boost: TP clearly dominates FP
        if config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0 {
            // Track A rubric gate: refuse calibrator severity bumps that the
            // finding's own text doesn't justify under the rubric (CLAUDE.md
            // severity_rubric). Bump-suppression preserves the original
            // severity but still records the confirmation. Default ON; set
            // `QUORUM_RUBRIC_GATE=off` (or `0`) to disable for ablation runs.
            let proposed = boost_severity(&finding.severity);
            let gate_on = std::env::var("QUORUM_RUBRIC_GATE")
                .map(|v| v != "off" && v != "0")
                .unwrap_or(true);
            if !gate_on || rubric_supports_severity_bump(&proposed, &finding) {
                finding.severity = proposed;
                boosted += 1;
            }
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        } else if tp_weight > fp_weight * 1.5 {
            // Confirm only when TP meaningfully outweighs FP
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        }
        // Mixed signal (TP ~ FP): leave calibrator_action as None

        traces.push(crate::calibrator_trace::CalibratorTraceEntry {
            finding_title: finding.title.clone(),
            finding_category: finding.category.clone(),
            tp_weight,
            fp_weight,
            wontfix_weight,
            full_suppress_weight,
            soft_fp_weight,
            matched_precedents,
            action: finding.calibrator_action.clone(),
            input_severity,
            output_severity: finding.severity.clone(),
            severity_change_reason: None,
        });

        output.push(finding);
    }

    CalibrationResult {
        findings: output,
        suppressed,
        boosted,
        traces,
    }
}

/// Calibrate findings using a FeedbackIndex for similarity matching.
/// Uses semantic embeddings when available, falls back to Jaccard.
/// This is the preferred path when a FeedbackIndex has been built.
/// Extract the numeric metric from a complexity-style finding title.
/// Returns Some(N) for titles like "Function `foo` has cyclomatic complexity 11".
pub fn extract_complexity_metric(title: &str) -> Option<u32> {
    let lower = title.to_lowercase();
    let key = "complexity ";
    let pos = lower.find(key)?;
    let tail = &lower[pos + key.len()..];
    let num: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse().ok()
}

/// A precedent is metric-compatible with a finding if either:
/// - neither title has an extractable metric (no numeric constraint), OR
/// - both have metrics within an absolute gap of 2 (i.e. `|a - b| <= 2`).
///
/// An absolute threshold is stricter at low CC and more realistic at high CC
/// than a relative window — CC=10 vs CC=7 (gap=3) is rejected, CC=30 vs
/// CC=25 (gap=5) is rejected, CC=11 vs CC=10 (gap=1) is accepted.
pub fn precedent_metric_compatible(finding_title: &str, precedent_title: &str) -> bool {
    let f = extract_complexity_metric(finding_title);
    let p = extract_complexity_metric(precedent_title);
    match (f, p) {
        (Some(fn_), Some(pn)) => fn_.abs_diff(pn) <= 2,
        (None, None) => true,
        // One-sided metric mismatch: a complexity-specific finding must not
        // match a non-metric precedent (or vice versa). Without this guard a
        // CC=11 finding could be suppressed/boosted by a precedent like
        // "Function `foo` is unused", which has nothing to do with complexity.
        (Some(_), None) | (None, Some(_)) => false,
    }
}

pub fn calibrate_with_index(
    findings: Vec<Finding>,
    index: &mut crate::feedback_index::FeedbackIndex,
    config: &CalibratorConfig,
) -> CalibrationResult {
    if index.is_empty() {
        return CalibrationResult { findings, suppressed: 0, boosted: 0, traces: vec![] };
    }
    // Ablation knob: bypass calibrator post-processing entirely. Used by the
    // calibrator-eval harness to isolate few-shot effects from post-hoc
    // severity bumps. Returns findings unchanged.
    if std::env::var("QUORUM_DISABLE_CALIBRATOR").is_ok() {
        return CalibrationResult { findings, suppressed: 0, boosted: 0, traces: vec![] };
    }

    // Capture `now` once so every verdict_weight invocation in this calibration
    // uses the same reference time (#123 Layer 1 — clock injection refactor).
    let now = chrono::Utc::now();

    let mut output = Vec::new();
    let mut suppressed = 0;
    let mut boosted = 0;
    let mut traces = Vec::new();

    for mut finding in findings {
        let input_severity = finding.severity.clone();
        // Feed hydration-rich discriminators to the index so paraphrased
        // precedents disambiguate on concrete tokens (function names, sink
        // keywords, framework references) instead of just title overlap.
        let first_evidence = finding.evidence.first().map(String::as_str).unwrap_or("");
        let excerpt = finding.based_on_excerpt.as_deref().unwrap_or("");
        let discriminators: [&str; 3] = [&finding.description, first_evidence, excerpt];
        // Pull a deeper candidate pool than we ultimately consume so the
        // downstream provenance and metric-compatibility filters don't starve
        // calibration when the top-k is dominated by auto-calibrate or
        // off-metric precedents. The post-filter is used only for weight
        // accumulation; there is no per-finding cap that requires <=10.
        let similar_entries = index.find_similar_enriched(
            &finding.title,
            &finding.category,
            &discriminators,
            50,
        );

        // Filter by similarity threshold, provenance, and metric compatibility.
        // Metric filter: complexity findings must match precedents with a
        // comparable cyclomatic number. Without this, a CC=5 FP precedent at
        // embedding similarity 0.9 will wrongly suppress a real CC=11 finding.
        let finding_title_for_metric = finding.title.clone();
        let similar: Vec<&crate::feedback_index::SimilarEntry> = similar_entries.iter()
            .filter(|s| s.similarity >= config.embedding_similarity_threshold as f32)
            .filter(|s| {
                if config.use_auto_feedback { true }
                else { !matches!(s.entry.provenance, crate::feedback::Provenance::AutoCalibrate(_)) }
            })
            // OutOfScope FP exclusion (#123 Layer 1) — these represent "real
            // defect, tracked elsewhere", NOT suppression signal. Excluding
            // them from the precedent pool prevents legitimate findings from
            // being suppressed by deferrals.
            .filter(|s| {
                !matches!(
                    (&s.entry.verdict, &s.entry.fp_kind),
                    (Verdict::Fp, Some(crate::feedback::FpKind::OutOfScope { .. }))
                )
            })
            .filter(|s| precedent_metric_compatible(&finding_title_for_metric, &s.entry.finding_title))
            .collect();

        if similar.is_empty() {
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight: 0.0,
                fp_weight: 0.0,
                wontfix_weight: 0.0,
                full_suppress_weight: 0.0,
                soft_fp_weight: 0.0,
                matched_precedents: vec![],
                action: None,
                input_severity: input_severity.clone(),
                output_severity: finding.severity.clone(),
                severity_change_reason: None,
            });
            output.push(finding);
            continue;
        }

        // Provenance-bucketed, per-bucket-capped weights. Weights here are
        // scaled by embedding similarity before bucketing.
        let tp_weight = accumulate_capped(
            similar
                .iter()
                .filter(|s| matches!(s.entry.verdict, Verdict::Tp | Verdict::Partial))
                .map(|s| (&s.entry.provenance, verdict_weight(&s.entry, now) * s.similarity as f64)),
        );
        let fp_weight = accumulate_capped(
            similar
                .iter()
                .filter(|s| s.entry.verdict == Verdict::Fp)
                .map(|s| (&s.entry.provenance, verdict_weight(&s.entry, now) * s.similarity as f64)),
        );

        // Wontfix weight — retained only for trace diagnostics. Wontfix no longer
        // contributes to soft or full suppression (see inertness rationale below).
        let mut wontfix_weight: f64 = 0.0;
        for s in similar.iter().filter(|s| s.entry.verdict == Verdict::Wontfix) {
            let w = verdict_weight(&s.entry, now) * s.similarity as f64;
            wontfix_weight += w;
        }
        // Wontfix is inert: pre-existing issues the user chose not to fix carry no
        // signal about finding validity. Excluded from both soft and full suppression.
        let soft_fp_weight = fp_weight;

        // Build precedent traces for this finding
        let matched_precedents: Vec<crate::calibrator_trace::PrecedentTrace> = similar
            .iter()
            .map(|s| crate::calibrator_trace::PrecedentTrace {
                finding_title: s.entry.finding_title.clone(),
                verdict: s.entry.verdict.clone(),
                similarity: s.similarity as f64,
                // Must match decision math: verdict_weight * similarity (see TP/FP/wontfix
                // accumulation above). Storing verdict_weight alone silently under-reports
                // near-miss precedents during debugging.
                weight: verdict_weight(&s.entry, now) * s.similarity as f64,
                provenance: serde_json::to_string(&s.entry.provenance).unwrap_or_default(),
                file_path: s.entry.file_path.clone(),
            })
            .collect();

        // Annotate with precedent info
        for s in &similar {
            finding.similar_precedent.push(format!(
                "{}: {} ({}) [sim={:.2}]",
                match s.entry.verdict {
                    Verdict::Tp => "TP", Verdict::Fp => "FP",
                    Verdict::Partial => "Partial", Verdict::Wontfix => "Wontfix",
                    Verdict::ContextMisleading { .. } => "ContextMisleading",
                },
                s.entry.finding_title, s.entry.reason, s.similarity
            ));
        }

        // Full suppress: FP weight only. Wontfix no longer contributes.
        let full_suppress_weight = fp_weight;
        if full_suppress_weight >= 1.5 && fp_weight > 0.0 && full_suppress_weight > tp_weight * 2.0 {
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            traces.push(crate::calibrator_trace::CalibratorTraceEntry {
                finding_title: finding.title.clone(),
                finding_category: finding.category.clone(),
                tp_weight,
                fp_weight,
                wontfix_weight,
                full_suppress_weight,
                soft_fp_weight,
                matched_precedents,
                action: finding.calibrator_action.clone(),
                input_severity,
                output_severity: finding.severity.clone(),
                severity_change_reason: None,
            });
            suppressed += 1;
            continue;
        }

        // Soft suppress: FP weight only (wontfix is inert), or auto-only FP
        // This preserves the finding for human review while reducing noise
        // Two triggers: (a) strong FP dominates TP; (b) modest FP, ~zero TP.
        if (soft_fp_weight >= 1.0 && soft_fp_weight > tp_weight * 2.0)
            || (soft_fp_weight >= 0.5 && tp_weight < 0.1)
        {
            finding.severity = Severity::Info;
            finding.calibrator_action = Some(CalibratorAction::Disputed);
            // Don't increment suppressed — finding stays in output at reduced severity
        }

        // Boost: TP clearly dominates FP
        if config.boost_tp && tp_weight >= 1.5 && tp_weight > fp_weight * 2.0 {
            // Track A rubric gate: refuse calibrator severity bumps that the
            // finding's own text doesn't justify under the rubric (CLAUDE.md
            // severity_rubric). Bump-suppression preserves the original
            // severity but still records the confirmation. Default ON; set
            // `QUORUM_RUBRIC_GATE=off` (or `0`) to disable for ablation runs.
            let proposed = boost_severity(&finding.severity);
            let gate_on = std::env::var("QUORUM_RUBRIC_GATE")
                .map(|v| v != "off" && v != "0")
                .unwrap_or(true);
            if !gate_on || rubric_supports_severity_bump(&proposed, &finding) {
                finding.severity = proposed;
                boosted += 1;
            }
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        } else if tp_weight > fp_weight * 1.5 {
            // Confirm only when TP meaningfully outweighs FP
            finding.calibrator_action = Some(CalibratorAction::Confirmed);
        }
        // Mixed signal (TP ~ FP): leave calibrator_action as None

        traces.push(crate::calibrator_trace::CalibratorTraceEntry {
            finding_title: finding.title.clone(),
            finding_category: finding.category.clone(),
            tp_weight,
            fp_weight,
            wontfix_weight,
            full_suppress_weight,
            soft_fp_weight,
            matched_precedents,
            action: finding.calibrator_action.clone(),
            input_severity,
            output_severity: finding.severity.clone(),
            severity_change_reason: None,
        });

        output.push(finding);
    }

    CalibrationResult { findings: output, suppressed, boosted, traces }
}

fn boost_severity(severity: &Severity) -> Severity {
    match severity {
        Severity::Info => Severity::Low,
        Severity::Low => Severity::Medium,
        Severity::Medium => Severity::High,
        Severity::High => Severity::Critical,
        Severity::Critical => Severity::Critical,
    }
}

/// Track A rubric gate: does the finding's own evidence justify a calibrator
/// severity bump TO `target`? Without this, vote-weighted severity can mimic
/// the corpus's severity distribution instead of the rubric, producing the
/// inflation pattern observed in the 2026-05-01 4-arm ablation (calibrator-only
/// arm bumped 6 HIGH → 14 HIGH on a 7-file sample, with ≥3 finds plainly
/// violating the "stylistic concerns are never high" rubric clause).
///
/// Gate fires only when `target` is High or Critical:
/// - `Critical`: requires explicit rubric-clause keywords (RCE / auth bypass /
///   credential leak / data corruption / guaranteed crash) in the finding's
///   title or description.
/// - `High`: stylistic categories (style/complexity/performance/maintainability/
///   api-design) only reach High when the text cites a concrete bug they hide.
///   Other categories (security/logic/concurrency/etc.) reach High freely —
///   the LLM's category assignment is the gating signal there.
/// - Below High: no gate.
///
/// Returning `false` means the calibrator must NOT apply the bump (keep the
/// LLM's emitted severity). The calibrator can still mark the finding as
/// confirmed; only the severity-raise is suppressed.
fn rubric_supports_severity_bump(target: &Severity, finding: &Finding) -> bool {
    let haystack = format!("{} {}", finding.title, finding.description).to_lowercase();
    let cat = finding.category.to_lowercase();

    // Word-boundary aware keyword check: a keyword matches if every char on
    // either side of an occurrence is non-alphanumeric (or start/end of
    // string). This avoids false matches like "rce" inside "force" — the
    // exact bug that prompted this gate's first iteration: a "brute force"
    // mention on an MD5 finding tripped the "rce" keyword and let the bump
    // through.
    fn contains_word(haystack: &str, needle: &str) -> bool {
        let nlen = needle.len();
        if nlen == 0 || haystack.len() < nlen {
            return false;
        }
        let bytes = haystack.as_bytes();
        let mut i = 0;
        while i + nlen <= bytes.len() {
            if &bytes[i..i + nlen] == needle.as_bytes() {
                let left_ok = i == 0 || !(bytes[i - 1] as char).is_alphanumeric();
                let right_ok = i + nlen == bytes.len()
                    || !(bytes[i + nlen] as char).is_alphanumeric();
                if left_ok && right_ok {
                    return true;
                }
            }
            i += 1;
        }
        false
    }

    match target {
        Severity::Critical => {
            // CRITICAL = data corruption, RCE, auth bypass, credential leak,
            // or guaranteed production crash. Require evidence in the text.
            const CRITICAL_KEYWORDS: &[&str] = &[
                "rce",
                "remote code execution",
                "code execution",
                "arbitrary code",
                "data corruption",
                "auth bypass",
                "authentication bypass",
                "credential leak",
                "credential exfil",
                "secret leak",
                "guaranteed crash",
                "guaranteed production crash",
                "data loss",
            ];
            CRITICAL_KEYWORDS.iter().any(|k| contains_word(&haystack, k))
        }
        Severity::High => {
            // Allowlist of categories that may freely promote medium → high.
            // Anything else (quality, lint, ast-pattern, unknown, bug,
            // best-practices, etc.) requires explicit bug-evidence keywords.
            // Inversion (was: blocklist of 5 stylistic categories) per
            // 2026-05-01 gpt-5.5 review: a blocklist silently lets every
            // unrecognized category through, defeating the gate.
            const HIGH_FREE_CATEGORIES: &[&str] = &[
                "security",
                "correctness",
                "logic",
                "concurrency",
                "reliability",
                "robustness",
                "error-handling",
                "error handling",
                "validation",
            ];
            // Compound categories like "security/correctness" qualify if the
            // first segment is in the allowlist — be liberal about the head.
            let head = cat.split(&['/', ',', ' '][..]).next().unwrap_or(&cat);
            let allow_free = HIGH_FREE_CATEGORIES
                .iter()
                .any(|c| *c == cat.as_str() || *c == head);
            if allow_free {
                true
            } else {
                const HIGH_EVIDENCE_KEYWORDS: &[&str] = &[
                    "hides a bug",
                    "hidden bug",
                    "race condition",
                    "resource leak",
                    "memory leak",
                    "exploit",
                    "vulnerability",
                    "injection",
                    "data race",
                    "use-after-free",
                    "null deref",
                    "null dereference",
                    "buffer overflow",
                    "unsafe",
                ];
                HIGH_EVIDENCE_KEYWORDS.iter().any(|k| contains_word(&haystack, k))
            }
        }
        // Medium and below: no gate
        _ => true,
    }
}

/// Compute similarity between a finding and a feedback entry.
/// Weighted combination of title similarity and category match.
fn finding_feedback_similarity(finding: &Finding, entry: &FeedbackEntry) -> f64 {
    let mut score = 0.0;

    // Title word overlap (Jaccard) — weight 3
    let title_sim = word_jaccard(&finding.title, &entry.finding_title);
    score += title_sim * 3.0;

    // Category exact match — weight 2
    if !entry.finding_category.is_empty() && finding.category == entry.finding_category {
        score += 2.0;
    }

    score / 5.0
}

fn word_jaccard(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    // Case-insensitive: feedback reasons are typed inconsistently ("SQL" vs "sql").
    // Lowering here keeps the set tokens equivalent without affecting display.
    fn tokens(s: &str) -> std::collections::HashSet<String> {
        s.split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    }
    let words_a = tokens(a);
    let words_b = tokens(b);
    let intersection = words_a.intersection(&words_b).count() as f64;
    let union = words_a.union(&words_b).count() as f64;
    if union == 0.0 { 0.0 } else { intersection / union }
}

// ---------------------------------------------------------------------------
// Per-chunk injection threshold escalation (Task 8.3)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

/// Stateful calibrator that tracks per-chunk ContextMisleading confirmations
/// and escalates the retrieval injection threshold accordingly.
///
/// This is a new, retrieval-focused companion to the existing free-function
/// `calibrate`/`calibrate_with_index` API; those paths are intentionally
/// untouched. The retriever (Task 8.4) consults
/// [`Calibrator::injection_threshold_for`] before deciding whether to inject a
/// chunk. Each `Verdict::ContextMisleading` entry naming a chunk_id raises its
/// threshold linearly; after `inject_suppress_after` confirmations the chunk
/// is sealed at `f32::INFINITY` (fully suppressed).
#[derive(Debug, Clone)]
pub struct Calibrator {
    inject_floor: f32,
    inject_suppress_after: u32,
    misleading_counts: HashMap<String, u32>,
}

impl Calibrator {
    /// Build a Calibrator with the global inject floor (typically
    /// `ContextConfig::inject_min_score`) and the default suppression budget
    /// (`CalibratorConfig::default().inject_suppress_after`).
    pub fn new(inject_floor: f32) -> Self {
        let defaults = CalibratorConfig::default();
        Self {
            inject_floor,
            inject_suppress_after: defaults.inject_suppress_after,
            misleading_counts: HashMap::new(),
        }
    }

    /// Build a Calibrator and seed its per-chunk misleading index from a
    /// feedback-store snapshot. Every `Verdict::ContextMisleading` entry
    /// contributes one confirmation per blamed chunk_id.
    pub fn from_feedback(inject_floor: f32, feedback: &[FeedbackEntry]) -> Self {
        let mut cal = Self::new(inject_floor);
        for entry in feedback {
            if let Verdict::ContextMisleading { blamed_chunk_ids } = &entry.verdict {
                for chunk_id in blamed_chunk_ids {
                    *cal.misleading_counts.entry(chunk_id.clone()).or_insert(0) += 1;
                }
            }
        }
        cal
    }

    /// Override the suppression budget (default 3).
    pub fn with_suppress_after(mut self, n: u32) -> Self {
        self.inject_suppress_after = n.max(1);
        self
    }

    /// Record one ContextMisleading confirmation for `chunk_id`. Primarily for
    /// tests; the production path rebuilds state via [`Self::from_feedback`].
    pub(crate) fn record_misleading(&mut self, chunk_id: &str, _finding_title: &str) {
        *self.misleading_counts.entry(chunk_id.to_string()).or_insert(0) += 1;
    }

    /// Return the effective injection threshold for `chunk_id`.
    ///
    /// - No confirmations -> global floor.
    /// - `k` confirmations (`0 < k < N`) -> `floor + k * (1.0 - floor) / N`.
    /// - `k >= N` -> `f32::INFINITY` (fully suppressed).
    pub fn injection_threshold_for(&self, chunk_id: &str) -> f32 {
        let k = self.misleading_counts.get(chunk_id).copied().unwrap_or(0);
        let n = self.inject_suppress_after.max(1);
        if k >= n {
            return f32::INFINITY;
        }
        if k == 0 {
            return self.inject_floor;
        }
        let step = (1.0 - self.inject_floor) / n as f32;
        self.inject_floor + (k as f32) * step
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::FindingBuilder;
    use chrono::Utc;

    fn fb(title: &str, category: &str, verdict: Verdict) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: title.into(),
            finding_category: category.into(),
            verdict,
            reason: "test".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        }
    }

    fn finding_at(title: &str, category: &str, severity: Severity, description: &str) -> Finding {
        FindingBuilder::new()
            .title(title)
            .category(category)
            .severity(severity)
            .description(description)
            .build()
    }

    // --- Track A rubric gate tests ---

    #[test]
    fn rubric_gate_blocks_complexity_bump_to_high() {
        // Stylistic complexity finding at MEDIUM should NOT bump to HIGH —
        // the rubric forbids stylistic concerns above MEDIUM unless they
        // demonstrate a hidden bug. This was the most-replicated rubric
        // violation in the 2026-05-01 4-arm ablation.
        let f = finding_at(
            "Function `build_review_prompt` has cyclomatic complexity 22",
            "complexity",
            Severity::Medium,
            "The function body is 220 lines and branches deeply.",
        );
        assert!(
            !rubric_supports_severity_bump(&Severity::High, &f),
            "complexity-only finding must not justify HIGH severity"
        );
    }

    #[test]
    fn rubric_gate_blocks_ast_pattern_high_without_evidence() {
        // ast-grep findings can be anything — must require keyword evidence
        // before promoting to HIGH. Was free-passing under the prior blocklist
        // (caught by 2026-05-01 gpt-5.5 review).
        let f = finding_at(
            "subprocess-no-check: subprocess.run() without check=True",
            "ast-pattern",
            Severity::Medium,
            "Add check=True or inspect the return code.",
        );
        assert!(
            !rubric_supports_severity_bump(&Severity::High, &f),
            "generic ast-pattern finding must not auto-promote to HIGH"
        );
    }

    #[test]
    fn rubric_gate_allows_ast_pattern_high_with_evidence() {
        // Same category, but evidence keyword present → allowed.
        let f = finding_at(
            "block-on-in-async: calling `block_on` inside an async context",
            "ast-pattern",
            Severity::Medium,
            "Causes a deadlock or a data race in the runtime; use `.await`.",
        );
        assert!(
            rubric_supports_severity_bump(&Severity::High, &f),
            "ast-pattern finding citing a race condition must justify HIGH"
        );
    }

    #[test]
    fn rubric_gate_blocks_quality_and_unknown_categories_to_high() {
        for (title, cat, desc) in [
            ("Unused variable", "quality", "Cosmetic only."),
            ("Some bug", "bug", "Probably wrong."),
            ("Magic constant 100 reused", "best-practices", "Pull into constant."),
            ("Logging missing", "observability", "Add structured logging."),
            ("Some thing", "unknown", "Unclassified."),
        ] {
            let f = finding_at(title, cat, Severity::Medium, desc);
            assert!(
                !rubric_supports_severity_bump(&Severity::High, &f),
                "category `{cat}` must NOT auto-promote MEDIUM→HIGH without evidence keyword"
            );
        }
    }

    #[test]
    fn rubric_gate_compound_category_uses_head() {
        // "security/correctness" splits on '/'; head is "security" → allowed.
        let f = finding_at(
            "X is unsafe",
            "security/correctness",
            Severity::Medium,
            "An issue.",
        );
        assert!(
            rubric_supports_severity_bump(&Severity::High, &f),
            "compound category with allowlisted head must permit HIGH"
        );
        // Unknown head is blocked.
        let f2 = finding_at(
            "Y is wrong",
            "lint/quality",
            Severity::Medium,
            "An issue.",
        );
        assert!(
            !rubric_supports_severity_bump(&Severity::High, &f2),
            "compound category with non-allowlisted head must require evidence"
        );
    }

    #[test]
    fn rubric_gate_blocks_md5_bump_to_critical() {
        // MD5 hashing is "broken cryptographic primitive" → HIGH per rubric,
        // not CRITICAL. Without the gate, calibrator was bumping HIGH→CRITICAL
        // on this pattern (observed in 2026-05-01 ch-03 calibrated arm).
        let f = finding_at(
            "Passwords are hashed with unsalted MD5",
            "security",
            Severity::High,
            "User passwords use MD5, a fast broken hash function.",
        );
        assert!(
            !rubric_supports_severity_bump(&Severity::Critical, &f),
            "broken-crypto finding without credential-leak evidence must not justify CRITICAL"
        );
    }

    #[test]
    fn rubric_gate_allows_rce_bump_to_critical() {
        // ch-16 finding mentions "code execution" → rubric explicit "RCE" → CRITICAL.
        let f = finding_at(
            "Uploading into web root with weak extension blacklist enables code execution",
            "security",
            Severity::High,
            "The blocklist of `.jsp` is bypassable; uploaded `.jspx` files execute as JSP.",
        );
        assert!(
            rubric_supports_severity_bump(&Severity::Critical, &f),
            "explicit RCE evidence must justify CRITICAL"
        );
    }

    #[test]
    fn rubric_gate_allows_credential_leak_bump_to_critical() {
        let f = finding_at(
            "Unauthenticated admin endpoint leaks all stored password hashes",
            "security",
            Severity::High,
            "The /admin/usernames handler returns the entire userDatabase including credential leak.",
        );
        assert!(
            rubric_supports_severity_bump(&Severity::Critical, &f),
            "credential-leak evidence must justify CRITICAL"
        );
    }

    #[test]
    fn rubric_gate_allows_high_for_security_categories_without_keywords() {
        // Security/logic findings can reach HIGH freely — the LLM's category
        // assignment is the gating signal. Only stylistic categories require
        // bug-keyword evidence to reach HIGH.
        let f = finding_at(
            "Race condition in shared HashMap",
            "concurrency",
            Severity::Medium,
            "Spring controllers are singletons; concurrent writes can corrupt the map.",
        );
        assert!(
            rubric_supports_severity_bump(&Severity::High, &f),
            "non-stylistic category must allow HIGH bump without explicit keywords"
        );
    }

    #[test]
    fn rubric_gate_does_not_block_below_high() {
        // Bumps to MEDIUM, LOW, INFO are not gated.
        let f = finding_at(
            "Function `foo` has cyclomatic complexity 8",
            "complexity",
            Severity::Low,
            "A simple style observation.",
        );
        assert!(rubric_supports_severity_bump(&Severity::Medium, &f));
        assert!(rubric_supports_severity_bump(&Severity::Low, &f));
        assert!(rubric_supports_severity_bump(&Severity::Info, &f));
    }

    #[test]
    fn rubric_gate_brute_force_does_not_match_rce_keyword() {
        // Regression: word-boundary check. "brute force" used to match the
        // "rce" keyword (substring of "force"), letting the calibrator bump
        // MD5 password-hashing findings to CRITICAL. Caught in the
        // 2026-05-01 4-arm gate validation run.
        let f = finding_at(
            "Passwords are hashed with unsalted MD5",
            "security",
            Severity::High,
            "MD5 is broken for password storage. Leaked hashes can be cracked \
             with precomputed tables or GPU brute force.",
        );
        assert!(
            !rubric_supports_severity_bump(&Severity::Critical, &f),
            "'brute force' must not match 'rce' keyword via substring"
        );
    }

    #[test]
    fn rubric_gate_word_boundary_blocks_substring_matches() {
        // Word-boundary check: "exploitation" must not match the "exploit"
        // keyword (which is the stylistic-HIGH list, not Critical, but the
        // same word-boundary helper services both). The gate fires only on
        // a stylistic category, so use complexity here.
        let f = finding_at(
            "Function `f` has cyclomatic complexity 30",
            "complexity",
            Severity::Medium,
            "The function is hard to maintain. Post-exploitation cleanup also \
             happens in the same scope, contributing to size.",
        );
        // "post-exploitation" must NOT trigger the "exploit" stylistic-HIGH
        // keyword (otherwise we'd let complexity findings reach HIGH on any
        // tangential mention of an exploit).
        assert!(
            !rubric_supports_severity_bump(&Severity::High, &f),
            "'exploitation' must not match 'exploit' via substring inside a stylistic finding"
        );
    }

    #[test]
    fn verdict_weight_future_dated_entry_is_not_max_weight() {
        // Regression: `(now - timestamp).num_days().max(0)` clamped negative
        // ages to 0 for future-dated entries (clock skew, manual JSONL edits),
        // giving them maximum recency weight. Use absolute age so a year-future
        // entry decays the same as a year-old one.
        let now = Utc::now();
        let mut future = fb("anything", "x", Verdict::Tp);
        future.timestamp = now + chrono::Duration::days(365);
        let w_future = verdict_weight(&future, now);

        let mut fresh = fb("anything", "x", Verdict::Tp);
        fresh.timestamp = now;
        let w_now = verdict_weight(&fresh, now);

        assert!(
            w_future < w_now * 0.1,
            "future-dated entry weight {w_future} should decay below 10% of fresh weight {w_now}"
        );
    }

    // -------------------------------------------------------------------
    // #123 Layer 1 — per-kind recency in verdict_weight (Task 3 RED)
    // -------------------------------------------------------------------

    /// Pinned timestamp for deterministic per-kind weight tests. Wall-clock
    /// independence is mandatory — recency is sensitive to small drift.
    fn pinned_now() -> chrono::DateTime<Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-05-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn fp_kind_trust_model_decays_3x_faster() {
        // 40-day-old TrustModelAssumption weight ≈ 120-day-old Hallucination
        // weight (both at e^-1 ≈ 0.368). Plus second assertion proving the
        // 40d branch fired (kills "both arms collapsed" mutant).
        let now = pinned_now();
        let trust = FeedbackEntry {
            file_path: "f".into(),
            finding_title: "t".into(),
            finding_category: "".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: now - chrono::Duration::days(40),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::TrustModelAssumption),
        };
        let halluc_120d = FeedbackEntry {
            timestamp: now - chrono::Duration::days(120),
            fp_kind: Some(crate::feedback::FpKind::Hallucination),
            ..trust.clone()
        };
        let halluc_40d = FeedbackEntry {
            timestamp: now - chrono::Duration::days(40),
            fp_kind: Some(crate::feedback::FpKind::Hallucination),
            ..trust.clone()
        };
        let trust_w = verdict_weight(&trust, now);
        let halluc_120d_w = verdict_weight(&halluc_120d, now);
        let halluc_40d_w = verdict_weight(&halluc_40d, now);

        let ratio = trust_w / halluc_120d_w;
        assert!(
            (0.95..=1.05).contains(&ratio),
            "TrustModelAssumption@40d should ≈ Hallucination@120d; ratio={}, trust={}, halluc_120d={}",
            ratio, trust_w, halluc_120d_w,
        );
        // Anchor: prove the 40d branch fired — same age, different decay.
        assert!(
            trust_w < halluc_40d_w,
            "TrustModel@40d ({}) must decay faster than Hallucination@40d ({})",
            trust_w, halluc_40d_w,
        );
        // Absolute-value lock for the 40d τ constant (kills 40.0→39.0 mutant).
        // 1.0 (Human) * e^(-40/40) = e^-1 ≈ 0.36788.
        assert!(
            (0.366..=0.370).contains(&trust_w),
            "TrustModel@40d must equal 1.0 * e^-1 ≈ 0.368; got {} (40d τ may have drifted)",
            trust_w,
        );
    }

    /// #123 Layer 1 (Task 4): regression locks. Hallucination, CompensatingControl,
    /// and None all route through the default 120d branch with tight tolerances
    /// so a `120.0 → 119.0` mutant gets killed.
    #[test]
    fn fp_kind_hallucination_default_recency_120d() {
        let now = pinned_now();
        let entry = FeedbackEntry {
            file_path: "f".into(),
            finding_title: "t".into(),
            finding_category: "".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: now - chrono::Duration::days(120),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::Hallucination),
        };
        let w = verdict_weight(&entry, now);
        // 1.0 (Human) * e^-1 ≈ 0.36788 — tight tolerance kills 120→119 mutant.
        assert!((0.366..=0.370).contains(&w), "expected ≈0.368, got {}", w);
    }

    #[test]
    fn fp_kind_compensating_control_keeps_120d_recency() {
        let now = pinned_now();
        let entry = FeedbackEntry {
            file_path: "f".into(),
            finding_title: "t".into(),
            finding_category: "".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: now - chrono::Duration::days(120),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::CompensatingControl {
                reference: "PR #99".into(),
            }),
        };
        let w = verdict_weight(&entry, now);
        assert!((0.366..=0.370).contains(&w), "expected ≈0.368 (120d), got {}", w);
    }

    #[test]
    fn fp_kind_none_routes_to_default_branch() {
        // Negative anchor: a 100d-old None-kind FP must weigh STRICTLY MORE
        // than a 100d-old TrustModelAssumption FP. Proves None routes to the
        // 120d default arm, not coincidentally to the 40d trust-model arm.
        let now = pinned_now();
        let none_entry = FeedbackEntry {
            file_path: "f".into(),
            finding_title: "t".into(),
            finding_category: "".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: now - chrono::Duration::days(100),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };
        let trust_entry = FeedbackEntry {
            fp_kind: Some(crate::feedback::FpKind::TrustModelAssumption),
            ..none_entry.clone()
        };
        let none_w = verdict_weight(&none_entry, now);
        let trust_w = verdict_weight(&trust_entry, now);
        assert!(
            none_w > trust_w,
            "None must route to 120d default; none={} should exceed trust@40d={}",
            none_w, trust_w,
        );
        // Spot check absolute value: 1.0 * e^(-100/120) ≈ 0.4346.
        assert!(
            (0.433..=0.436).contains(&none_w),
            "None@100d expected ≈0.4346, got {}",
            none_w,
        );
    }

    /// #123 Layer 1 (Task 9): regression lock for verdict-coupling.
    ///
    /// `fp_kind` is meaningful only for `Verdict::Fp`. The exhaustive match
    /// in `verdict_weight` already pins this — the catch-all arm
    /// `(Tp | Partial | Wontfix | ContextMisleading {..}, _) => 120.0`
    /// ignores `fp_kind` for non-Fp verdicts. This test exists so a future
    /// refactor that drops the catch-all and accidentally consults `fp_kind`
    /// for TP entries fails immediately. Passes by construction today.
    #[test]
    fn fp_kind_ignored_on_tp_verdict() {
        let now = pinned_now();
        let entry = FeedbackEntry {
            file_path: "f".into(),
            finding_title: "t".into(),
            finding_category: "".into(),
            verdict: Verdict::Tp,
            reason: "r".into(),
            model: None,
            timestamp: now - chrono::Duration::days(40),
            provenance: crate::feedback::Provenance::Human,
            // If this entry's fp_kind ever leaks into recency-tau selection,
            // we'd get the 40d trust-model branch — weight ≈ 1.0 * e^(-1) =
            // 0.368. The regression lock asserts we land on the 120d default.
            fp_kind: Some(crate::feedback::FpKind::TrustModelAssumption),
        };
        let w = verdict_weight(&entry, now);
        // 1.0 (Human) * e^(-40/120) ≈ 0.7165 — uses 120d default branch.
        assert!(
            (0.715..=0.718).contains(&w),
            "fp_kind must be ignored when verdict != Fp; expected ≈0.717, got {}",
            w,
        );
    }

    /// #123 Layer 1 (Task 5): OutOfScope FPs are excluded from the precedent
    /// pool entirely — they represent "real defect, tracked elsewhere", NOT
    /// suppression signal. Pair this with `fp_kind_hallucination_does_suppress_precedent_pool`
    /// (positive control) so we know the absence-of-suppression isn't because
    /// suppression itself is broken.
    #[test]
    fn fp_kind_out_of_scope_excluded_from_precedent_pool() {
        let make_oos = |idx: i64| FeedbackEntry {
            file_path: "src/foo.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: format!("OutOfScope #{}", idx),
            model: None,
            timestamp: chrono::Utc::now() - chrono::Duration::days(idx),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::OutOfScope {
                tracked_in: Some(format!("#{}", idx)),
            }),
        };
        let feedback = vec![make_oos(1), make_oos(2), make_oos(3)];
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(
            result.findings.len(), 1,
            "OutOfScope FPs must NOT suppress (they're 'real, tracked elsewhere', not 'wrong')"
        );
        assert_eq!(result.suppressed, 0);
    }

    /// POSITIVE CONTROL for fp_kind_out_of_scope_excluded_from_precedent_pool.
    /// Same body, only the kind differs. With Hallucination, the FPs DO
    /// suppress (existing calibrator behavior). Without this control passing,
    /// the OutOfScope test could pass for the wrong reason (e.g. suppression
    /// itself broken, threshold misconfigured).
    #[test]
    fn fp_kind_hallucination_does_suppress_precedent_pool() {
        let make_halluc = |idx: i64| FeedbackEntry {
            file_path: "src/foo.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: format!("Hallucination #{}", idx),
            model: None,
            timestamp: chrono::Utc::now() - chrono::Duration::days(idx),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::Hallucination),
        };
        let feedback = vec![make_halluc(1), make_halluc(2), make_halluc(3)];
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        // 3 same-title Hallucination FPs MUST suppress (existing behavior).
        // If this fails, the OutOfScope test above is meaningless — it could
        // be passing because suppression itself is broken.
        assert_eq!(
            result.findings.len(), 0,
            "control: 3 Hallucination FPs MUST suppress; if this fails, suppression itself is broken"
        );
        assert_eq!(result.suppressed, 1);
    }

    /// #123 Layer 1 (PAL parity recommendation): both calibrator paths must
    /// reach the same suppression decision on identical corpora. `calibrate()`
    /// excludes OutOfScope at filter-build time, while `calibrate_with_index()`
    /// excludes per-finding after `find_similar_enriched` has already pulled
    /// candidates from the embedding/jaccard index. Functionally equivalent
    /// today; this test pins it so a future index-level truncation can't
    /// silently diverge them.
    #[test]
    fn calibrate_paths_agree_on_out_of_scope_exclusion() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));
        let make_oos = |idx: i64| FeedbackEntry {
            file_path: "src/foo.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: format!("OutOfScope #{}", idx),
            model: None,
            timestamp: chrono::Utc::now() - chrono::Duration::days(idx),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: Some(crate::feedback::FpKind::OutOfScope {
                tracked_in: Some(format!("#{}", idx)),
            }),
        };
        let entries = vec![make_oos(1), make_oos(2), make_oos(3)];
        for e in &entries {
            store.record(e).unwrap();
        }

        let make_finding = || FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .build();
        let config = CalibratorConfig::default();

        // Path A: calibrate() — in-memory feedback slice.
        let result_a = calibrate(vec![make_finding()], &entries, &config);

        // Path B: calibrate_with_index() — same corpus via Jaccard index
        // (deterministic; doesn't depend on embedding model availability).
        let mut index = crate::feedback_index::FeedbackIndex::build_jaccard_only(&store).unwrap();
        let result_b = calibrate_with_index(vec![make_finding()], &mut index, &config);

        assert_eq!(
            result_a.findings.len(), result_b.findings.len(),
            "calibrate() and calibrate_with_index() must agree on OutOfScope exclusion; \
             A.findings={}, B.findings={}",
            result_a.findings.len(), result_b.findings.len(),
        );
        assert_eq!(
            result_a.suppressed, result_b.suppressed,
            "suppress count must match across paths; A.suppressed={}, B.suppressed={}",
            result_a.suppressed, result_b.suppressed,
        );
        // Both paths must let the finding survive (OutOfScope is non-suppressive).
        assert_eq!(result_a.findings.len(), 1, "path A: OutOfScope must not suppress");
        assert_eq!(result_b.findings.len(), 1, "path B: OutOfScope must not suppress");
    }

    // -- No feedback: passthrough --

    #[test]
    fn calibrator_config_has_separate_thresholds() {
        let config = CalibratorConfig::default();
        assert!(config.embedding_similarity_threshold > config.similarity_threshold,
            "Embedding threshold should be higher than Jaccard threshold");
    }

    #[test]
    fn embedding_threshold_admits_moderately_similar_precedents() {
        // bge-small-en cosine routinely sits in 0.72-0.78 for paraphrased-but-semantically-identical
        // findings (e.g. "SQL injection via f-string" vs "SQL injection using string formatting").
        // A threshold of 0.80 was empirically too strict — real precedents kept missing their
        // matches, leading to the March→April precision regression.
        let config = CalibratorConfig::default();
        assert!(config.embedding_similarity_threshold <= 0.75,
            "embedding threshold {} excludes legitimate paraphrases — should be <= 0.75",
            config.embedding_similarity_threshold);
    }

    #[test]
    fn calibrate_no_feedback_passthrough() {
        let findings = vec![
            FindingBuilder::new().title("Bug A").build(),
            FindingBuilder::new().title("Bug B").build(),
        ];
        let result = calibrate(findings, &[], &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 2);
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.boosted, 0);
    }

    // -- FP suppression --

    #[test]
    fn calibrate_suppresses_finding_with_fp_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Fp),
            fb("SQL injection", "security", Verdict::Fp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 0, "Finding should be suppressed by 2 FP precedents");
        assert_eq!(result.suppressed, 1);
    }

    #[test]
    fn calibrate_does_not_suppress_with_insufficient_fp() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Fp),
            // Only 1 FP, need 2 to suppress
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.suppressed, 0);
    }

    #[test]
    fn calibrate_marks_suppressed_as_disputed() {
        let findings = vec![
            FindingBuilder::new()
                .title("Unused import")
                .category("style")
                .build(),
        ];
        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import", "style", Verdict::Fp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.suppressed, 1);
    }

    // -- TP boosting --

    #[test]
    fn calibrate_boosts_severity_with_tp_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("Buffer overflow")
                .category("security")
                .severity(Severity::Medium)
                .build(),
        ];
        let feedback = vec![
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
        assert!(result.findings[0].severity > Severity::Medium, "Should be boosted above Medium");
        assert_eq!(result.boosted, 1);
    }

    #[test]
    fn calibrate_no_boost_when_disabled() {
        let findings = vec![
            FindingBuilder::new()
                .title("Buffer overflow")
                .category("security")
                .severity(Severity::Medium)
                .build(),
        ];
        let feedback = vec![
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
        ];
        let config = CalibratorConfig {
            boost_tp: false,
            ..Default::default()
        };
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.findings[0].severity, Severity::Medium);
        assert_eq!(result.boosted, 0);
    }

    #[test]
    fn calibrate_env_disable_short_circuits_non_indexed_path() {
        // Regression: QUORUM_DISABLE_CALIBRATOR was only honored in
        // calibrate_with_index. Quorum self-review (2026-05-01) caught
        // that the non-indexed `calibrate()` would still suppress / boost,
        // so ablation runs that fell back to it produced misleading data.
        let findings = vec![
            FindingBuilder::new()
                .title("Buffer overflow")
                .category("security")
                .severity(Severity::Medium)
                .build(),
        ];
        let feedback = vec![
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
            fb("Buffer overflow", "security", Verdict::Tp),
        ];
        // SAFETY: tests run single-threaded for env mutations in this crate
        // (no parallel test runner config); reset after the assertion.
        unsafe {
            std::env::set_var("QUORUM_DISABLE_CALIBRATOR", "1");
        }
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        unsafe {
            std::env::remove_var("QUORUM_DISABLE_CALIBRATOR");
        }
        assert_eq!(result.findings[0].severity, Severity::Medium, "must not boost when disabled");
        assert_eq!(result.boosted, 0);
        assert_eq!(result.suppressed, 0);
        assert!(result.traces.is_empty(), "no traces should be emitted when disabled");
    }

    // -- Mixed precedent --

    #[test]
    fn calibrate_mixed_tp_fp_uses_majority() {
        let findings = vec![
            FindingBuilder::new()
                .title("Race condition")
                .category("concurrency")
                .build(),
        ];
        let feedback = vec![
            fb("Race condition", "concurrency", Verdict::Tp),
            fb("Race condition", "concurrency", Verdict::Tp),
            fb("Race condition", "concurrency", Verdict::Fp),
        ];
        // 2 TP vs 1 FP = keep (and possibly boost)
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings.len(), 1);
    }

    // -- Similarity matching --

    #[test]
    fn word_jaccard_is_case_insensitive() {
        // HTTP/framework terminology is typed inconsistently in feedback reasons —
        // "SQL injection" vs "sql injection" vs "SQL Injection". Case-sensitive
        // matching silently drops precedent matches that humans would treat as
        // identical. Gemini 3 Pro flagged this as a calibrator leak.
        let a = "SQL Injection via f-string formatting";
        let b = "sql injection via f-string formatting";
        let score = word_jaccard(a, b);
        assert!((score - 1.0).abs() < 1e-9,
            "case-only difference should score 1.0, got {}", score);
    }

    #[test]
    fn similarity_exact_match() {
        let finding = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .build();
        let entry = fb("SQL injection", "security", Verdict::Tp);
        assert!(finding_feedback_similarity(&finding, &entry) > 0.8);
    }

    #[test]
    fn similarity_different_finding() {
        let finding = FindingBuilder::new()
            .title("SQL injection in auth module")
            .category("security")
            .build();
        let entry = fb("Unused import os", "style", Verdict::Fp);
        assert!(finding_feedback_similarity(&finding, &entry) < 0.3);
    }

    #[test]
    fn similarity_partial_title_match() {
        let finding = FindingBuilder::new()
            .title("SQL injection via string concatenation")
            .category("security")
            .build();
        let entry = fb("SQL injection in query builder", "security", Verdict::Tp);
        let sim = finding_feedback_similarity(&finding, &entry);
        assert!(sim > 0.4 && sim < 0.9, "Partial match should be moderate: {}", sim);
    }

    // -- Precedent annotation --

    #[test]
    fn calibrate_annotates_findings_with_precedent() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert!(!result.findings[0].similar_precedent.is_empty(),
            "Finding should have precedent annotation");
    }

    #[test]
    fn calibrator_excludes_auto_feedback_when_configured() {
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let auto_fb = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
            fp_kind: None,
        };
        let feedback = vec![auto_fb.clone(), auto_fb];
        let config = CalibratorConfig {
            use_auto_feedback: false,
            ..Default::default()
        };
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 0, "Auto feedback excluded, should not suppress");
    }

    #[test]
    fn calibrator_includes_auto_feedback_by_default() {
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let auto_fb = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
            fp_kind: None,
        };
        let human_fb = FeedbackEntry {
            provenance: crate::feedback::Provenance::Human,
            reason: "confirmed".into(),
            ..auto_fb.clone()
        };
        // 2 auto FPs (capped at 1.0) + 1 human FP (1.0) = 2.0 >= 1.5 -> suppress
        let feedback = vec![auto_fb.clone(), auto_fb, human_fb];
        let config = CalibratorConfig::default(); // use_auto_feedback defaults to true
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 1, "Auto+human feedback should suppress (auto capped at 1.0 + human 1.0 = 2.0)");
    }

    #[test]
    fn calibrate_confirmed_action_on_tp_match() {
        let findings = vec![
            FindingBuilder::new()
                .title("Null pointer")
                .category("safety")
                .build(),
        ];
        let feedback = vec![
            fb("Null pointer", "safety", Verdict::Tp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }

    // -- Weighted scoring tests --

    #[test]
    fn weighted_calibrator_human_feedback_counts_more() {
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];

        let human_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: "handled upstream".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };
        let auto_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
            fp_kind: None,
        };

        // Human (1.0) + auto (0.5) = 1.5 >= threshold -> suppress
        let config = CalibratorConfig::default();
        let result1 = calibrate(findings.clone(), &vec![human_fp.clone(), auto_fp.clone()], &config);
        assert_eq!(result1.suppressed, 1, "Human+auto FP should suppress");

        // 2 auto only: 0.5 + 0.5 = 1.0 < 1.5 threshold -> NOT suppress
        let result2 = calibrate(findings.clone(), &vec![auto_fp.clone(), auto_fp], &config);
        assert_eq!(result2.suppressed, 0, "2 auto FPs alone should not suppress (insufficient weight)");
    }

    #[test]
    fn weighted_calibrator_recency_matters() {
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let old_fp = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "old".into(),
            model: None,
            timestamp: Utc::now() - chrono::Duration::days(90),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };
        let recent_fp = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "recent".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };

        let config = CalibratorConfig::default();
        // 2 recent human FPs: 1.0 + 1.0 = 2.0 >= 1.5 -> suppress
        let result1 = calibrate(findings.clone(), &vec![recent_fp.clone(), recent_fp], &config);
        assert_eq!(result1.suppressed, 1);

        // 2 old FPs: exp(-90/60) ~= 0.22 each, total ~0.44 < 1.5 -> NOT suppress
        let result2 = calibrate(findings.clone(), &vec![old_fp.clone(), old_fp], &config);
        assert!(result2.suppressed <= 1);
    }

    #[test]
    fn weighted_calibrator_produces_confidence() {
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];
        let tp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "real".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![tp], &config);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }

    #[test]
    fn recency_weight_90_day_old_still_meaningful() {
        let old_entry = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "old".into(),
            model: None,
            timestamp: Utc::now() - chrono::Duration::days(90),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        };
        let weight = verdict_weight(&old_entry, Utc::now());
        assert!(weight >= 0.3,
            "90-day-old human feedback should retain >= 30% weight, got {:.3}", weight);
    }

    #[test]
    fn postfix_provenance_has_highest_weight() {
        // A single PostFix TP (weight 1.5) should be enough to confirm
        let findings = vec![FindingBuilder::new().title("SQL injection").category("security").build()];
        let postfix_tp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "fixed with parameterized queries".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::PostFix,
            fp_kind: None,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![postfix_tp], &config);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Confirmed));
    }

    #[test]
    fn auto_calibrate_weight_capped_at_one() {
        // 4 auto FPs: uncapped = 4 * 0.5 = 2.0 (would suppress)
        // capped = min(2.0, 1.0) = 1.0 (should NOT fully suppress — needs human corroboration)
        // Note: soft suppression downgrades severity to INFO but finding remains in output
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let auto_fb = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
            fp_kind: None,
        };
        let feedback = vec![auto_fb.clone(), auto_fb.clone(), auto_fb.clone(), auto_fb];
        let config = CalibratorConfig::default();
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 0,
            "4 auto FPs should not suppress (capped at 1.0 weight, needs human corroboration)");
    }

    #[test]
    fn auto_plus_human_still_suppresses() {
        // 2 auto FPs (capped at 1.0) + 1 human FP (1.0) = 2.0 >= 1.5 -> suppress
        let findings = vec![FindingBuilder::new().title("Bug").category("test").build()];
        let auto_fb = FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "Bug".into(),
            finding_category: "test".into(),
            verdict: Verdict::Fp,
            reason: "auto".into(),
            model: Some("o3".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate("o3".into()),
            fp_kind: None,
        };
        let human_fb = FeedbackEntry {
            provenance: crate::feedback::Provenance::Human,
            reason: "confirmed FP".into(),
            ..auto_fb.clone()
        };
        let feedback = vec![auto_fb.clone(), auto_fb, human_fb];
        let config = CalibratorConfig::default();
        let result = calibrate(findings, &feedback, &config);
        assert_eq!(result.suppressed, 1,
            "Auto (capped 1.0) + human (1.0) = 2.0 should suppress");
    }

    #[test]
    fn auto_only_fp_soft_suppresses_to_info() {
        // Auto-only FP should downgrade to INFO, not fully suppress
        let finding = FindingBuilder::new()
            .title("Template uses states() without availability check")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
        ];
        // Make all entries auto-calibrate provenance
        let auto_feedback: Vec<FeedbackEntry> = feedback.into_iter().map(|mut e| {
            e.provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
            e
        }).collect();

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &auto_feedback, &config);

        // Finding should NOT be fully suppressed
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        // But should be downgraded to INFO
        assert_eq!(result.findings[0].severity, Severity::Info);
        assert_eq!(result.findings[0].calibrator_action, Some(CalibratorAction::Disputed));
    }

    #[test]
    fn auto_plus_human_fp_still_fully_suppresses() {
        // Human corroboration should still enable full suppression
        let finding = FindingBuilder::new()
            .title("Template uses states() without availability check")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let mut feedback = vec![
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
            fb("Template uses states() without availability check", "quality", Verdict::Fp),
        ];
        // One auto, one human — human provides the extra weight to cross 1.5
        feedback[0].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[1].provenance = crate::feedback::Provenance::Human;

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // Should be fully suppressed (human corroboration)
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn auto_fp_with_tp_opposition_no_soft_suppress() {
        // If there's significant TP signal, don't soft suppress even with auto FP
        let finding = FindingBuilder::new()
            .title("Use of unwrap() may panic")
            .severity(Severity::Medium)
            .category("security")
            .build();
        let mut feedback = vec![
            fb("Use of unwrap() may panic", "security", Verdict::Fp),
            fb("Use of unwrap() may panic", "security", Verdict::Fp),
            fb("Use of unwrap() may panic", "security", Verdict::Tp),
        ];
        feedback[0].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[1].provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
        feedback[2].provenance = crate::feedback::Provenance::Human;

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // Mixed signal — should NOT be soft suppressed
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        assert_ne!(result.findings[0].severity, Severity::Info); // severity preserved
    }

    #[test]
    fn soft_suppress_preserves_finding_in_output() {
        // Soft-suppressed findings must remain in output (not filtered out)
        let finding = FindingBuilder::new()
            .title("Deprecated trigger syntax")
            .severity(Severity::High)
            .category("quality")
            .build();
        let auto_feedback: Vec<FeedbackEntry> = (0..5).map(|_| {
            let mut e = fb("Deprecated trigger syntax", "quality", Verdict::Fp);
            e.provenance = crate::feedback::Provenance::AutoCalibrate("gpt-5.4".into());
            e
        }).collect();

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &auto_feedback, &config);

        assert_eq!(result.findings.len(), 1); // still in output
        assert_eq!(result.findings[0].severity, Severity::Info); // but downgraded
    }

    #[test]
    fn postfix_fp_suppresses_with_single_entry() {
        // A single PostFix FP (weight 1.5) meets the 1.5 threshold alone
        let findings = vec![FindingBuilder::new().title("Unused import").category("style").build()];
        let postfix_fp = FeedbackEntry {
            file_path: "test.py".into(),
            finding_title: "Unused import".into(),
            finding_category: "style".into(),
            verdict: Verdict::Fp,
            reason: "import is used dynamically".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::PostFix,
            fp_kind: None,
        };

        let config = CalibratorConfig::default();
        let result = calibrate(findings, &vec![postfix_fp], &config);
        assert_eq!(result.suppressed, 1, "Single PostFix FP should suppress (weight 1.5 >= threshold)");
    }

    #[test]
    fn wontfix_alone_is_inert_no_suppression() {
        // Wontfix often means "real issue but we're not touching it" — carrying no signal
        // about whether the finding itself is valid. So it must be inert: no full-suppress,
        // no soft-suppress, no severity change.
        let finding = FindingBuilder::new()
            .title("console.log debug artifact")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
            fb("console.log debug artifact", "quality", Verdict::Wontfix),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        assert_eq!(result.suppressed, 0, "wontfix should NOT fully suppress");
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Medium,
            "wontfix should not change severity — it's inert like Partial");
    }

    #[test]
    fn fp_still_fully_suppresses() {
        let finding = FindingBuilder::new()
            .title("console.log debug artifact")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        let feedback = vec![
            fb("console.log debug artifact", "quality", Verdict::Fp),
            fb("console.log debug artifact", "quality", Verdict::Fp),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        assert_eq!(result.suppressed, 1, "FP should fully suppress");
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn mixed_fp_wontfix_fp_drives_suppress() {
        let finding = FindingBuilder::new()
            .title("unused variable")
            .severity(Severity::Medium)
            .category("quality")
            .build();
        // 2 FP (enough for full suppress) + 1 wontfix
        let feedback = vec![
            fb("unused variable", "quality", Verdict::Fp),
            fb("unused variable", "quality", Verdict::Fp),
            fb("unused variable", "quality", Verdict::Wontfix),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);

        // FP alone should drive full suppression
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.findings.len(), 0);
    }

    #[test]
    fn wontfix_does_not_help_fp_reach_full_suppress() {
        // One FP alone is below the 1.5 threshold. Wontfix must NOT tip it over —
        // previously wontfix contributed at 50%, which was the bug: pre-existing
        // untouched issues shouldn't vote for suppression.
        let finding = FindingBuilder::new()
            .title("Missing explicit mode")
            .category("quality")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("Missing explicit mode", "quality", Verdict::Fp),
            fb("Missing explicit mode defaults", "quality", Verdict::Wontfix),
            fb("No explicit mode set", "quality", Verdict::Wontfix),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 0, "1 FP + wontfix should NOT reach full suppress threshold");
        assert_eq!(result.findings.len(), 1);
    }

    #[test]
    fn wontfix_alone_is_inert_even_with_many_entries() {
        // Even a large pile of wontfix precedents must not suppress or downgrade —
        // they only tell us "the user isn't fixing this right now", not that the
        // finding itself is wrong or noise.
        let finding = FindingBuilder::new()
            .title("No explicit mode")
            .category("quality")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("No explicit mode", "quality", Verdict::Wontfix),
            fb("No explicit mode set", "quality", Verdict::Wontfix),
            fb("Missing explicit mode", "quality", Verdict::Wontfix),
            fb("Automation has no mode", "quality", Verdict::Wontfix),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 0);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].severity, Severity::Medium,
            "wontfix is inert — must not downgrade severity");
    }

    #[test]
    fn calibrate_populates_trace_for_suppressed_finding() {
        let finding = FindingBuilder::new()
            .title("Unused import")
            .category("style")
            .severity(Severity::Low)
            .build();

        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
            fb("Unused import os", "style", Verdict::Fp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.suppressed, 1);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.finding_title, "Unused import");
        assert!(trace.fp_weight > 0.0);
        assert_eq!(trace.action, Some(CalibratorAction::Disputed));
        assert!(!trace.matched_precedents.is_empty());
    }

    #[test]
    fn calibrate_populates_trace_for_boosted_finding() {
        let finding = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .severity(Severity::Medium)
            .build();

        let feedback = vec![
            fb("SQL injection", "security", Verdict::Tp),
            fb("SQL injection in query", "security", Verdict::Tp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.action, Some(CalibratorAction::Confirmed));
        assert!(trace.tp_weight > 0.0);
        assert_eq!(trace.input_severity, Severity::Medium);
        assert_eq!(trace.output_severity, Severity::High);
    }

    #[test]
    fn calibrate_populates_trace_for_passthrough() {
        let finding = FindingBuilder::new()
            .title("Race condition")
            .category("concurrency")
            .build();

        let feedback = vec![
            fb("Unused import", "style", Verdict::Fp),
        ];

        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.traces.len(), 1);

        let trace = &result.traces[0];
        assert_eq!(trace.finding_title, "Race condition");
        assert_eq!(trace.tp_weight, 0.0);
        assert_eq!(trace.fp_weight, 0.0);
        assert!(trace.matched_precedents.is_empty());
        assert_eq!(trace.action, None);
    }

    // ── Metric-aware precedent filtering ──

    #[test]
    fn extract_complexity_metric_parses_cc() {
        assert_eq!(
            extract_complexity_metric("Function `foo` has cyclomatic complexity 11"),
            Some(11)
        );
        assert_eq!(
            extract_complexity_metric("Function `bar` has cyclomatic complexity 5"),
            Some(5)
        );
        assert_eq!(
            extract_complexity_metric("open-no-encoding: missing encoding"),
            None
        );
    }

    #[test]
    fn extract_complexity_metric_handles_non_ascii() {
        // Title contains multi-byte chars; must not panic even if lowercasing
        // shifts byte offsets (e.g. Turkish `İ` -> `i̇` changes length).
        assert_eq!(
            extract_complexity_metric("İstanbul function has cyclomatic complexity 11"),
            Some(11)
        );
        // Non-ASCII without the keyword -> None, still no panic.
        assert_eq!(extract_complexity_metric("函数 has no metric"), None);
    }

    #[test]
    fn precedent_metric_close_values_compatible() {
        // CC=11 vs CC=10 -- 9% gap, well within window
        assert!(precedent_metric_compatible(
            "Function `x` has cyclomatic complexity 11",
            "Function `y` has cyclomatic complexity 10"
        ));
    }

    #[test]
    fn precedent_metric_incompatible_large_gap() {
        assert!(!precedent_metric_compatible(
            "Function `x` has cyclomatic complexity 11",
            "Function `y` has cyclomatic complexity 5"
        ));
        assert!(!precedent_metric_compatible(
            "Function `x` has cyclomatic complexity 11",
            "Function `y` has cyclomatic complexity 6"
        ));
        assert!(!precedent_metric_compatible(
            "Function `big` has cyclomatic complexity 30",
            "Function `small` has cyclomatic complexity 6"
        ));
    }

    #[test]
    fn precedent_metric_uses_absolute_gap_not_relative() {
        // Absolute threshold |a-b| < 3 so CC=10 vs CC=7 is rejected (gap=3)
        // This matters for moderate-CC findings where a 30% relative window
        // still admits noisy precedents.
        assert!(
            !precedent_metric_compatible(
                "Function `x` has cyclomatic complexity 10",
                "Function `y` has cyclomatic complexity 7"
            ),
            "CC=10 vs CC=7 (gap=3) must be rejected under absolute threshold"
        );
        // CC=30 vs CC=25 (gap=5) rejected under absolute, though 17% relative.
        assert!(
            !precedent_metric_compatible(
                "Function `a` has cyclomatic complexity 30",
                "Function `b` has cyclomatic complexity 25"
            ),
            "CC=30 vs CC=25 (gap=5) must be rejected"
        );
        // Within absolute threshold: gap=2
        assert!(precedent_metric_compatible(
            "Function `x` has cyclomatic complexity 11",
            "Function `y` has cyclomatic complexity 9"
        ));
    }

    #[test]
    fn precedent_metric_compatible_when_no_metric() {
        // Non-complexity findings: no metric constraint applies
        assert!(precedent_metric_compatible(
            "SQL injection in query builder",
            "SQL injection risk"
        ));
    }

    #[test]
    fn precedent_metric_compatible_rejects_one_sided_metric_mismatch() {
        // Regression: previously the `_ => true` arm allowed a metric finding
        // to match a non-metric precedent (and vice versa). A CC=11 complexity
        // finding must not be calibrated by an unrelated "function is unused"
        // precedent that happens to share the same function name.
        assert!(!precedent_metric_compatible(
            "Function `foo` has cyclomatic complexity 11",
            "Function `foo` is unused"
        ));
        assert!(!precedent_metric_compatible(
            "Function `foo` is unused",
            "Function `foo` has cyclomatic complexity 11"
        ));
    }

    #[test]
    fn calibrate_downgrades_clear_fp_no_tp_to_info() {
        // Observed case (from real review of script.js): finding "Function X has
        // cyclomatic complexity 13" with ONE FP precedent at similarity ~0.83.
        // fp_weight ~= 0.83 * 1.0 (human) = 0.83. No TP precedents.
        // Current logic requires soft_fp_weight >= 1.0 so this passes through
        // at Medium severity unchanged. With clear FP signal and no TP, we
        // should downgrade to Info.
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));
        // Slightly different title -> similarity ~0.83, fp_weight ~0.83.
        store.record(&fb(
            "Function `helper` has cyclomatic complexity 12",
            "complexity",
            Verdict::Fp,
        )).unwrap();
        let mut index = crate::feedback_index::FeedbackIndex::build(&store).unwrap();
        let finding = FindingBuilder::new()
            .title("Function `createFood` has cyclomatic complexity 13")
            .category("complexity")
            .severity(crate::finding::Severity::Medium)
            .build();
        let config = CalibratorConfig::default();
        let result = calibrate_with_index(vec![finding], &mut index, &config);
        assert_eq!(result.findings.len(), 1);
        let out = &result.findings[0];
        assert_eq!(
            out.severity,
            crate::finding::Severity::Info,
            "clear-FP-no-TP finding should be downgraded to Info"
        );
        assert_eq!(
            out.calibrator_action,
            Some(crate::finding::CalibratorAction::Disputed),
            "downgraded finding should be marked Disputed"
        );
    }

    #[test]
    fn calibrate_ignores_low_cc_fp_precedent_for_high_cc_finding() {
        // CC=11 finding must not receive FP weight from CC=5 precedents.
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));
        for e in [
            fb("Function `simple` has cyclomatic complexity 5", "complexity", Verdict::Fp),
            fb("Function `tiny` has cyclomatic complexity 6", "complexity", Verdict::Fp),
            fb("Function `tiny2` has cyclomatic complexity 4", "complexity", Verdict::Fp),
        ] {
            store.record(&e).unwrap();
        }
        let mut index = crate::feedback_index::FeedbackIndex::build(&store).unwrap();
        let finding = FindingBuilder::new()
            .title("Function `bigfn` has cyclomatic complexity 11")
            .category("complexity")
            .severity(crate::finding::Severity::Medium)
            .build();
        let config = CalibratorConfig::default();
        let result = calibrate_with_index(vec![finding], &mut index, &config);
        assert_eq!(result.findings.len(), 1);
        let trace = &result.traces[0];
        assert_eq!(
            trace.fp_weight, 0.0,
            "CC=5/6/4 FP precedents must NOT contribute to CC=11 finding's fp_weight"
        );
        assert!(
            trace.matched_precedents.is_empty(),
            "metric-incompatible precedents should be filtered out before weighting"
        );
    }

    #[test]
    fn enrichment_separates_jwt_validation_from_generic_input_validation() {
        // The exact conflation Gemini 3 Pro flagged: generic-validation FP in
        // the feedback store must NOT suppress a new JWT-signature-validation
        // finding. Before enrichment (v0.13.2), these two patterns clustered
        // in bge-small cosine space because the title-only embedding dropped
        // the distinguishing tokens. Now corpus carries `reason` and query
        // carries `description + evidence + based_on_excerpt`, so they
        // should separate without needing a discriminator gate.
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));

        // Two FP entries about generic API input validation — enough weight to
        // fully suppress IF they match.
        let mut fp1 = fb("Missing input validation", "security", Verdict::Fp);
        fp1.reason = "API endpoint parameters not validated before DB write — \
                      handled by pydantic models downstream".into();
        let mut fp2 = fb("Missing input validation", "security", Verdict::Fp);
        fp2.reason = "request body type checks missing — already covered by \
                      FastAPI response_model".into();
        store.record(&fp1).unwrap();
        store.record(&fp2).unwrap();

        // New finding: JWT signature validation — completely different concern.
        let jwt_finding = FindingBuilder::new()
            .title("Missing input validation")
            .description("decode_token() does not verify the JWT signature \
                          algorithm claim, allowing HS256 tokens signed with \
                          untrusted keys to bypass jwt.verify checks")
            .category("security")
            .severity(Severity::High)
            .evidence("jwt.verify(token, secret, { algorithms: ['HS256'] })")
            .build();

        let mut index = crate::feedback_index::FeedbackIndex::build_jaccard_only(&store).unwrap();
        let config = CalibratorConfig::default();
        let result = calibrate_with_index(vec![jwt_finding], &mut index, &config);

        // Diagnostic trace output (visible with --nocapture).
        let trace = &result.traces[0];
        eprintln!("\n=== enrichment probe ===");
        eprintln!("TP weight: {:.3}  FP weight: {:.3}  full_suppress: {:.3}",
            trace.tp_weight, trace.fp_weight, trace.full_suppress_weight);
        for p in &trace.matched_precedents {
            eprintln!("  precedent: sim={:.3} weight={:.3} verdict={:?} reason={}",
                p.similarity, p.weight, p.verdict, p.finding_title);
        }
        eprintln!("action: {:?}", trace.action);
        eprintln!("output severity: {:?} (input High)\n", result.findings.get(0).map(|f| f.severity.clone()));

        // Core assertion: JWT finding must not be suppressed (output or disputed).
        assert_eq!(result.suppressed, 0,
            "JWT finding must NOT be suppressed by generic input-validation FPs. \
             full_suppress_weight={:.3}", trace.full_suppress_weight);
        // And severity should stay High (not downgraded to Info).
        assert_eq!(result.findings[0].severity, Severity::High,
            "severity must stay High; was downgraded, indicating FP precedent match");
    }

    #[test]
    fn legacy_trace_records_per_entry_similarity_not_one() {
        // Legacy (non-index) path was recording similarity=1.0 for every precedent,
        // masking whether a near-miss or exact match drove the calibration decision.
        // Paired bug with the embedding-path weight fix.
        let finding = FindingBuilder::new()
            .title("Missing input validation on webhook handler")
            .category("security")
            .severity(Severity::High)
            .build();
        let feedback = vec![
            fb("Missing input validation", "security", Verdict::Fp),
        ];
        let config = CalibratorConfig::default();
        let result = calibrate(vec![finding], &feedback, &config);
        let trace = &result.traces[0];
        let prec = trace.matched_precedents.first()
            .expect("should have a matched precedent");
        assert!(prec.similarity < 1.0,
            "legacy trace similarity should reflect actual Jaccard, got {}",
            prec.similarity);
    }

    #[test]
    fn embedding_trace_weight_matches_decision_math() {
        // Decisions in calibrate_with_index use `verdict_weight(entry) * similarity`
        // (lines ~328/341/354). The trace output must record that SAME value — not
        // just verdict_weight(entry) without similarity — so operators debugging a
        // suppression can see the actual contribution. Flagged by Gemini 3 Pro as
        // a calibrator observability bug.
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));
        // Jaccard-only index avoids fastembed flakiness: with deliberately
        // non-overlapping titles, similarity stays well below 1.0 so the
        // multiplier matters.
        store.record(&fb("Missing input validation on endpoint", "security", Verdict::Tp)).unwrap();
        let mut index = crate::feedback_index::FeedbackIndex::build_jaccard_only(&store).unwrap();

        let finding = FindingBuilder::new()
            .title("Missing input validation on webhook handler endpoint")
            .category("security")
            .severity(crate::finding::Severity::High)
            .build();
        let mut config = CalibratorConfig::default();
        // Lower threshold for this test so the sub-1.0 Jaccard similarity clears the gate.
        config.embedding_similarity_threshold = 0.3;
        let result = calibrate_with_index(vec![finding], &mut index, &config);
        let trace = &result.traces[0];
        let prec = trace.matched_precedents.first()
            .expect("precedent should be present when index has a matching entry");
        assert!(prec.similarity < 1.0,
            "test fixture should yield a sub-1.0 similarity, got {}", prec.similarity);
        let expected = prec.similarity * verdict_weight(&store.load_all().unwrap()[0], Utc::now());
        assert!((prec.weight - expected).abs() < 1e-6,
            "trace weight {} must equal verdict_weight * similarity ({}); \
             decisions use the product, trace must match",
            prec.weight, expected);
    }

    #[test]
    fn legacy_calibrate_ignores_metric_incompatible_fp_precedent() {
        // Legacy calibrate() path (no embedding index) must also drop
        // metric-incompatible precedents. CC=11 finding vs CC=5/6/4 FPs.
        let feedback = vec![
            fb("Function `simple` has cyclomatic complexity 5", "complexity", Verdict::Fp),
            fb("Function `tiny` has cyclomatic complexity 6", "complexity", Verdict::Fp),
            fb("Function `tiny2` has cyclomatic complexity 4", "complexity", Verdict::Fp),
        ];
        let finding = FindingBuilder::new()
            .title("Function `bigfn` has cyclomatic complexity 11")
            .category("complexity")
            .severity(crate::finding::Severity::Medium)
            .build();
        let result = calibrate(vec![finding], &feedback, &CalibratorConfig::default());
        let trace = &result.traces[0];
        assert_eq!(
            trace.fp_weight, 0.0,
            "legacy calibrate must reject CC=5/6/4 precedents for CC=11 finding"
        );
        assert_eq!(result.findings[0].severity, crate::finding::Severity::Medium,
            "metric-incompatible FPs must not downgrade severity");
    }

    // -- Per-chunk injection threshold escalation (Task 8.3) --

    fn misleading_entry(chunk_ids: &[&str]) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "foo.rs".into(),
            finding_title: "something that misled".into(),
            finding_category: "context".into(),
            verdict: Verdict::ContextMisleading {
                blamed_chunk_ids: chunk_ids.iter().map(|s| s.to_string()).collect(),
            },
            reason: "misleading".into(),
            model: Some("gpt-5.4".into()),
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Human,
            fp_kind: None,
        }
    }

    #[test]
    fn injection_threshold_is_global_floor_without_misleading_feedback() {
        let cal = Calibrator::new(0.65);
        assert!((cal.injection_threshold_for("never-seen") - 0.65).abs() < 1e-6);
    }

    #[test]
    fn threshold_rises_then_fully_suppresses_after_n_confirmations() {
        let mut cal = Calibrator::new(0.65);
        let id = "chunk-a";
        assert!((cal.injection_threshold_for(id) - 0.65).abs() < 1e-6);

        cal.record_misleading(id, "fp1");
        assert!(cal.injection_threshold_for(id) > 0.65);
        assert!(cal.injection_threshold_for(id).is_finite());

        cal.record_misleading(id, "fp2");
        assert!(cal.injection_threshold_for(id) > 0.65);
        assert!(cal.injection_threshold_for(id).is_finite());

        cal.record_misleading(id, "fp3");
        assert!(
            cal.injection_threshold_for(id).is_infinite(),
            "third confirmation must seal chunk at INF (got {})",
            cal.injection_threshold_for(id)
        );
    }

    #[test]
    fn threshold_uses_feedback_store_state_not_an_in_memory_counter() {
        // No in-test calls to record_misleading: threshold escalation must
        // come from the feedback snapshot supplied to the constructor.
        let feedback = vec![
            misleading_entry(&["chunk-a"]),
            misleading_entry(&["chunk-a"]),
        ];
        let cal = Calibrator::from_feedback(0.65, &feedback);
        let t = cal.injection_threshold_for("chunk-a");
        assert!(t > 0.65 && t.is_finite(), "expected raised but finite, got {t}");

        // One more confirmation in the store should seal it.
        let mut feedback = feedback;
        feedback.push(misleading_entry(&["chunk-a"]));
        let cal = Calibrator::from_feedback(0.65, &feedback);
        assert!(cal.injection_threshold_for("chunk-a").is_infinite());
    }

    #[test]
    fn unrelated_chunk_ids_stay_at_the_global_floor() {
        let feedback = vec![
            misleading_entry(&["chunk-a"]),
            misleading_entry(&["chunk-a"]),
            misleading_entry(&["chunk-a"]),
        ];
        let cal = Calibrator::from_feedback(0.65, &feedback);
        assert!(cal.injection_threshold_for("chunk-a").is_infinite());
        assert!((cal.injection_threshold_for("chunk-b") - 0.65).abs() < 1e-6);
    }

    #[test]
    fn threshold_for_chunk_blamed_by_non_context_misleading_entry_is_unchanged() {
        // A TP/FP/Wontfix verdict that happens to mention a chunk_id in its
        // finding_title must not consult the misleading-escalation path.
        let feedback = vec![
            fb("chunk-a was flagged", "context", Verdict::Tp),
            fb("chunk-a again",       "context", Verdict::Fp),
            fb("chunk-a ignored",     "context", Verdict::Wontfix),
        ];
        let cal = Calibrator::from_feedback(0.65, &feedback);
        assert!((cal.injection_threshold_for("chunk-a") - 0.65).abs() < 1e-6);
    }

    #[test]
    fn suppress_budget_is_configurable() {
        let mut cal = Calibrator::new(0.65).with_suppress_after(5);
        let id = "chunk-a";
        for _ in 0..4 {
            cal.record_misleading(id, "fp");
            assert!(cal.injection_threshold_for(id).is_finite());
        }
        cal.record_misleading(id, "fp");
        assert!(cal.injection_threshold_for(id).is_infinite());
    }

    // --- Task 2: External provenance weight (issue #32) ---

    #[test]
    fn external_provenance_weights_0_7() {
        use chrono::Utc;
        let entry = FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict: Verdict::Tp,
            reason: "r".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::External {
                agent: "pal".into(),
                model: None,
                confidence: None,
            },
            fp_kind: None,
        };
        let w = verdict_weight(&entry, Utc::now());
        assert!((w - 0.7).abs() < 0.01, "expected ~0.7, got {w}");
    }

    #[test]
    fn external_weight_independent_of_confidence_in_v1() {
        // confidence is stored but IGNORED by calibrator in v1.
        // Table-driven so one failure doesn't mask the others.
        use chrono::Utc;
        let mk = |conf: Option<f32>| FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict: Verdict::Tp,
            reason: "r".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::External {
                agent: "pal".into(),
                model: None,
                confidence: conf,
            },
            fp_kind: None,
        };
        let cases: &[(&str, Option<f32>)] = &[
            ("None", None),
            ("low", Some(0.1)),
            ("high", Some(0.99)),
            ("zero", Some(0.0)),
            ("one", Some(1.0)),
        ];
        for (label, conf) in cases {
            let w = verdict_weight(&mk(*conf), Utc::now());
            // Tolerance 1e-4: accommodates Utc::now() jitter between test-setup
            // and verdict_weight's internal clock read (flagged by quorum self-review).
            assert!(
                (w - 0.7).abs() < 1e-4,
                "confidence={label}: expected 0.7, got {w}"
            );
        }
    }

    #[test]
    fn unknown_weight_remains_0_3_regression_guard() {
        use chrono::Utc;
        let entry = FeedbackEntry {
            file_path: "a.rs".into(),
            finding_title: "t".into(),
            finding_category: "c".into(),
            verdict: Verdict::Tp,
            reason: "r".into(),
            model: None,
            timestamp: Utc::now(),
            provenance: crate::feedback::Provenance::Unknown,
            fp_kind: None,
        };
        let w = verdict_weight(&entry, Utc::now());
        assert!((w - 0.3).abs() < 0.01, "Unknown must stay at 0.3, got {w}");
    }

    // --- Task 3: External filter + uncapped bucket pinning (issue #32) ---

    /// External FP FeedbackEntry with a given age (days).
    fn external_fp(age_days: i64) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "src/auth.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Fp,
            reason: "r".into(),
            model: None,
            timestamp: Utc::now() - chrono::Duration::days(age_days),
            provenance: crate::feedback::Provenance::External {
                agent: "pal".into(),
                model: None,
                confidence: None,
            },
            fp_kind: None,
        }
    }

    #[test]
    fn external_not_filtered_when_use_auto_feedback_false() {
        // External must survive the use_auto_feedback=false filter that
        // specifically targets AutoCalibrate precedents.
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .severity(Severity::High)
                .build(),
        ];
        let feedback = vec![external_fp(0)];
        let config = CalibratorConfig {
            use_auto_feedback: false,
            ..Default::default()
        };
        let result = calibrate(findings, &feedback, &config);
        let trace = result.traces.last().expect("expected a calibrator trace");
        assert!(
            !trace.matched_precedents.is_empty(),
            "External verdict must survive use_auto_feedback=false"
        );
    }

    #[test]
    fn external_fp_accumulation_thresholds() {
        // Table-driven: one test covers n=1,2,3 with per-row failure messages.
        // Replaces a four-way split that duplicated setup and hid accumulator bugs.
        #[derive(Debug, PartialEq)]
        enum Outcome {
            Kept,
            Soft,
            Full,
        }

        // Calibrator soft-triggers when `soft_fp_weight >= 0.5 && tp_weight < 0.1`
        // (lightweight FP with no TP → already concerning). So 1 external FP
        // at 0.7 weight already trips the low soft trigger. Full-suppression
        // requires full_suppress_weight >= 1.5.
        //
        // Issue #97: External weights are now capped at EXTERNAL_WEIGHT_CAP
        // (1.4). Full suppression (>=1.5) is UNREACHABLE via External alone —
        // it requires humanish FP corroboration. The n=100 row locks this in.
        let cases: &[(usize, Outcome)] = &[
            (1, Outcome::Soft),   // 1 × 0.7 = 0.7: tp=0 → trips low soft (>=0.5)
            (2, Outcome::Soft),   // 2 × 0.7 = 1.4: soft (>=1.0), below cap
            (3, Outcome::Soft),   // 2.1 raw → capped at 1.4 → stays Soft (was Full pre-#97)
            (100, Outcome::Soft), // flood: cap holds; External alone can never trigger Full
        ];

        for (n, expected) in cases {
            let findings = vec![
                FindingBuilder::new()
                    .title("SQL injection")
                    .category("security")
                    .severity(Severity::High)
                    .build(),
            ];
            let feedback: Vec<_> = (0..*n as i64).map(external_fp).collect();
            let result = calibrate(findings, &feedback, &CalibratorConfig::default());
            let outcome = match (
                result.suppressed,
                result.findings.first().map(|f| &f.severity),
            ) {
                (1, _) => Outcome::Full,
                (0, Some(Severity::Info)) => Outcome::Soft,
                (0, Some(_)) => Outcome::Kept,
                _ => panic!("unexpected result for n={n}: suppressed={} findings={:?}", result.suppressed, result.findings),
            };
            assert_eq!(
                outcome, *expected,
                "n={n}: expected {expected:?}, got {outcome:?}"
            );
        }
    }

    // -- Issue #97: External accumulator cap --
    //
    // External-provenance entries (from other review agents) share the
    // `other_*_weight` bucket with Human and PostFix. Without a cap, a single
    // misbehaving or compromised agent can flood TP/FP verdicts and dominate
    // calibration. Per issue #97, we cap the global External sum at
    // `EXTERNAL_WEIGHT_CAP` (≈ 2 fresh External entries = 1 Human entry's
    // worth). Cap is global across agents, not per-agent.

    fn external_tp(age_days: i64) -> FeedbackEntry {
        FeedbackEntry {
            file_path: "test.rs".into(),
            finding_title: "SQL injection".into(),
            finding_category: "security".into(),
            verdict: Verdict::Tp,
            reason: "ext".into(),
            model: None,
            timestamp: Utc::now() - chrono::Duration::days(age_days),
            provenance: crate::feedback::Provenance::External {
                agent: "pal".into(),
                model: None,
                confidence: None,
            },
            fp_kind: None,
        }
    }

    fn external_tp_from(agent_name: &str, age_days: i64) -> FeedbackEntry {
        let mut e = external_tp(age_days);
        if let crate::feedback::Provenance::External { ref mut agent, .. } = e.provenance {
            *agent = agent_name.into();
        }
        e
    }

    #[test]
    fn external_tp_bucket_capped_at_constant() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback: Vec<_> = (0..10).map(|_| external_tp(0)).collect();
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (trace.tp_weight - EXTERNAL_WEIGHT_CAP).abs() < 1e-3,
            "expected tp_weight ≈ {} (got {}) — External bucket must be capped",
            EXTERNAL_WEIGHT_CAP,
            trace.tp_weight
        );
    }

    #[test]
    fn external_fp_bucket_capped_at_constant() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback: Vec<_> = (0..10)
            .map(|_| {
                let mut e = external_tp(0);
                e.verdict = Verdict::Fp;
                e
            })
            .collect();
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (trace.fp_weight - EXTERNAL_WEIGHT_CAP).abs() < 1e-3,
            "expected fp_weight ≈ {} (got {})",
            EXTERNAL_WEIGHT_CAP,
            trace.fp_weight
        );
    }

    #[test]
    fn external_below_cap_passes_through_unchanged() {
        // .min() must not floor: a single External at 0.7 stays at 0.7. Kills
        // a .min → .max mutant that would force every External up to the cap.
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![external_tp(0)];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (trace.tp_weight - 0.7).abs() < 1e-3,
            "expected tp_weight ≈ 0.7 (got {}) — below-cap values must pass through",
            trace.tp_weight
        );
    }

    #[test]
    fn humanish_bucket_remains_uncapped() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let mut feedback = Vec::new();
        for _ in 0..5 {
            feedback.push(fb("SQL injection", "security", Verdict::Tp));
        }
        for _ in 0..5 {
            let mut e = fb("SQL injection", "security", Verdict::Tp);
            e.provenance = crate::feedback::Provenance::PostFix;
            feedback.push(e);
        }
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            trace.tp_weight > EXTERNAL_WEIGHT_CAP + 5.0,
            "humanish bucket must NOT be capped; got tp_weight={} cap={}",
            trace.tp_weight,
            EXTERNAL_WEIGHT_CAP
        );
    }

    #[test]
    fn external_cap_is_global_across_agents() {
        // Per issue #97 spec: cap applies to the sum across all External
        // agents, not per-agent.
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let mut feedback: Vec<_> = (0..50).map(|_| external_tp_from("pal", 0)).collect();
        feedback.extend((0..50).map(|_| external_tp_from("third-opinion", 0)));
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (trace.tp_weight - EXTERNAL_WEIGHT_CAP).abs() < 1e-3,
            "cap must be global across agents; got tp_weight={} (cap={})",
            trace.tp_weight,
            EXTERNAL_WEIGHT_CAP
        );
    }

    #[test]
    fn humanish_empty_external_bucket_is_no_regression() {
        // Zero External entries: cap logic is a pure no-op.
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .category("security")
                .build(),
        ];
        let feedback = vec![
            fb("SQL injection", "security", Verdict::Fp),
            fb("SQL injection", "security", Verdict::Fp),
            fb("SQL injection", "security", Verdict::Fp),
        ];
        let result = calibrate(findings, &feedback, &CalibratorConfig::default());
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (2.95..=3.05).contains(&trace.fp_weight),
            "3 fresh Human FPs should give fp_weight ≈ 3.0 (got {})",
            trace.fp_weight
        );
    }

    #[test]
    fn external_cap_applies_in_calibrate_with_index_path() {
        // CodeRabbit-flagged: all the unit cap tests hit `calibrate()` (the
        // Jaccard path). `calibrate_with_index()` duplicates the cap math and
        // could silently diverge. `build_jaccard_only` sidesteps the embedding-
        // model download so this test is fast and hermetic.
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::feedback::FeedbackStore::new(dir.path().join("fb.jsonl"));
        for _ in 0..10 {
            store.record(&external_tp(0)).unwrap();
        }
        let mut index = crate::feedback_index::FeedbackIndex::build_jaccard_only(&store).unwrap();
        let config = CalibratorConfig {
            embedding_similarity_threshold: 0.0,
            ..Default::default()
        };
        let finding = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .build();
        let result = calibrate_with_index(vec![finding], &mut index, &config);
        let trace = result.traces.last().expect("expected trace");
        assert!(
            (trace.tp_weight - EXTERNAL_WEIGHT_CAP).abs() < 1e-3,
            "calibrate_with_index must also cap External; got tp_weight={} (cap={})",
            trace.tp_weight,
            EXTERNAL_WEIGHT_CAP
        );
    }
}
