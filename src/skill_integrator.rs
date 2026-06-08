//! Deterministic integrator stage for the skills framework (issue #411).
//!
//! Takes findings from all skill-matrix cells, clusters them by composite
//! key, merges overlapping findings within each cluster using noisy-or
//! confidence fusion, suppresses below a confidence floor, and produces
//! a deterministically sorted output.
//!
//! **No LLM calls** -- this module is pure, deterministic Rust.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::Utc;

use crate::finding::{Finding, Severity, new_finding_ulid};
use crate::skill_audit::{AuditWriter, ClusterKey, IntegratorDecision, IntegratorDecisionRecord};
use crate::skill_prompt_defense::sanitize_output;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default confidence floor: findings below this are suppressed.
const DEFAULT_CONFIDENCE_FLOOR: f64 = 0.30;

/// Default confidence assigned when `Finding.confidence` is `None`.
const DEFAULT_CONFIDENCE: f64 = 0.5;

// ---------------------------------------------------------------------------
// TaggedFinding
// ---------------------------------------------------------------------------

/// A finding annotated with the file path it originated from.
///
/// `Finding` does not carry a `file_path` field; the skill executor associates
/// findings with files through `CellSpec`. The integration layer must pair
/// each finding with its file path before passing to [`integrate`].
#[derive(Debug, Clone)]
pub struct TaggedFinding {
    pub file_path: String,
    pub finding: Finding,
}

// ---------------------------------------------------------------------------
// IntegratorConfig
// ---------------------------------------------------------------------------

/// Configuration for the integrator stage.
pub struct IntegratorConfig {
    /// Parent review run ID (ULID).
    pub run_id: String,
    /// Findings below this confidence are suppressed (default 0.30).
    pub confidence_floor: f64,
    /// Optional audit writer for `IntegratorDecisionRecord` rows.
    pub audit_writer: Option<Arc<AuditWriter<IntegratorDecisionRecord>>>,
}

impl Default for IntegratorConfig {
    fn default() -> Self {
        Self {
            run_id: new_finding_ulid(),
            confidence_floor: DEFAULT_CONFIDENCE_FLOOR,
            audit_writer: None,
        }
    }
}

// ---------------------------------------------------------------------------
// IntegratorOutput
// ---------------------------------------------------------------------------

/// Output of the integrator stage.
#[derive(Debug)]
pub struct IntegratorOutput {
    /// Findings that passed the confidence floor, sorted by severity desc,
    /// confidence desc, then (file_path, line_start).
    pub findings: Vec<Finding>,
    /// Findings that were suppressed (below confidence floor).
    pub suppressed: Vec<Finding>,
    /// All decision records (one per cluster).
    pub decisions: Vec<IntegratorDecisionRecord>,
}

// ---------------------------------------------------------------------------
// Primary clustering key
// ---------------------------------------------------------------------------

/// Primary composite key for clustering: `(file_path, finding_kind)`.
///
/// Two findings must share the same primary key to be considered for merging.
/// Within a primary key group, the secondary line-overlap check determines
/// whether they actually merge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct PrimaryKey {
    file_path: String,
    kind: String,
}

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

/// Extract the cluster kind key from a finding.
///
/// Returns `rule_id` if present (AST/linter findings), otherwise falls back
/// to [`normalize_title_slug`] on the finding's title.
pub(crate) fn finding_kind(finding: &Finding) -> String {
    if let Some(ref rule_id) = finding.rule_id
        && !rule_id.is_empty()
    {
        return rule_id.clone();
    }
    let slug = normalize_title_slug(&finding.title);
    if slug.is_empty() {
        return finding.title.to_lowercase();
    }
    slug
}

/// Produce a deterministic slug from a finding title.
///
/// Lowercase, ASCII alphanumeric + dashes only, vendor terms stripped,
/// runs of dashes collapsed, leading/trailing dashes trimmed.
pub(crate) fn normalize_title_slug(title: &str) -> String {
    // Vendor terms to strip (common LLM prefixes/suffixes that add noise).
    const VENDOR_TERMS: &[&str] = &[
        "potential",
        "possible",
        "detected",
        "warning",
        "error",
        "issue",
        "finding",
        "vulnerability",
    ];

    let lower = title.to_lowercase();

    // Replace non-ASCII-alphanumeric chars with dashes.
    let slugged: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Split on dashes, filter vendor terms, rejoin.
    let parts: Vec<&str> = slugged
        .split('-')
        .filter(|p| !p.is_empty() && !VENDOR_TERMS.contains(p))
        .collect();

    let joined = parts.join("-");

    // Collapse any remaining runs of dashes (defensive).
    collapse_dashes(&joined)
}

/// Collapse consecutive dashes into a single dash, trim leading/trailing.
fn collapse_dashes(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_dash = true; // true to trim leading dashes
    for c in s.chars() {
        if c == '-' {
            if !prev_was_dash {
                result.push('-');
                prev_was_dash = true;
            }
        } else {
            result.push(c);
            prev_was_dash = false;
        }
    }
    // Trim trailing dash.
    if result.ends_with('-') {
        result.pop();
    }
    result
}

/// Check whether two line ranges overlap by at least 50% of the shorter
/// range's length.
///
/// Single-line ranges `(n, n)` have length 1 and overlap if they share any
/// line. A range `(a, b)` has length `b - a + 1`.
pub(crate) fn ranges_overlap_enough(a: (u32, u32), b: (u32, u32)) -> bool {
    let a_start = a.0.min(a.1);
    let a_end = a.0.max(a.1);
    let b_start = b.0.min(b.1);
    let b_end = b.0.max(b.1);

    let overlap_start = a_start.max(b_start);
    let overlap_end = a_end.min(b_end);

    if overlap_start > overlap_end {
        return false;
    }
    let overlap_len = overlap_end - overlap_start + 1;

    let a_len = a_end - a_start + 1;
    let b_len = b_end - b_start + 1;
    let shorter = a_len.min(b_len);

    // 50% threshold: overlap >= shorter / 2, but use integer math to
    // avoid floating-point: overlap * 2 >= shorter.
    u64::from(overlap_len) * 2 >= u64::from(shorter)
}

// ---------------------------------------------------------------------------
// Noisy-or confidence fusion
// ---------------------------------------------------------------------------

/// Compute noisy-or fusion: `1 - product(1 - c_i * w_i)`.
///
/// Each entry is `(confidence, weight)`. Confidences and weights are clamped
/// to [0, 1] internally. The result is bounded [0, 1].
fn noisy_or(pairs: &[(f64, f64)]) -> f64 {
    let product: f64 = pairs
        .iter()
        .map(|&(c, w)| {
            let c = c.clamp(0.0, 1.0);
            let w = w.clamp(0.0, 1.0);
            1.0 - c * w
        })
        .product();
    (1.0 - product).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Ensemble dedup helper
// ---------------------------------------------------------------------------

/// Groups for ensemble dedup: same `originating_skill` across different
/// models should collapse into a single source before noisy-or.
///
/// Returns `(confidence, weight)` pairs ready for noisy-or, where ensemble
/// duplicates (same skill, different models) are collapsed by taking the max
/// confidence among them.
fn collapse_ensemble_duplicates(findings: &[&TaggedFinding]) -> Vec<(f64, f64)> {
    // Group by originating_skill. Findings without originating_skill each
    // get their own unique key.
    let mut skill_groups: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut anonymous_idx: u64 = 0;

    for tf in findings {
        let key = match &tf.finding.originating_skill {
            Some(skill) if !skill.is_empty() => skill.clone(),
            _ => {
                anonymous_idx += 1;
                format!("__anonymous_{anonymous_idx}")
            }
        };
        let conf = confidence_of(&tf.finding);
        skill_groups.entry(key).or_default().push(conf);
    }

    // For each skill group, take the max confidence. Weight is 1.0 for all
    // cross-skill contributions.
    skill_groups
        .values()
        .map(|confs| {
            let max_conf = confs.iter().copied().fold(0.0_f64, f64::max);
            (max_conf, 1.0)
        })
        .collect()
}

/// Extract confidence from a Finding as f64, defaulting to 0.5 if None.
fn confidence_of(f: &Finding) -> f64 {
    f.confidence
        .filter(|c| c.is_finite())
        .map(|c| f64::from(c.clamp(0.0, 1.0)))
        .unwrap_or(DEFAULT_CONFIDENCE)
}

/// Extract severity label as a String.
fn severity_label(s: &Severity) -> String {
    match s {
        Severity::Critical => "critical",
        Severity::High => "high",
        Severity::Medium => "medium",
        Severity::Low => "low",
        Severity::Info => "info",
    }
    .to_owned()
}

// ---------------------------------------------------------------------------
// integrate (main entry point)
// ---------------------------------------------------------------------------

/// Deterministic integrator: cluster, merge, suppress, sort.
///
/// Accepts tagged findings (finding + file_path) from all skill-matrix cells.
/// Findings are consumed (moved in).
pub fn integrate(
    tagged_findings: Vec<TaggedFinding>,
    config: &IntegratorConfig,
) -> IntegratorOutput {
    if tagged_findings.is_empty() {
        return IntegratorOutput {
            findings: vec![],
            suppressed: vec![],
            decisions: vec![],
        };
    }

    // Step 1: Group by primary key (file_path, finding_kind).
    let mut primary_groups: BTreeMap<PrimaryKey, Vec<TaggedFinding>> = BTreeMap::new();
    for tf in tagged_findings {
        let kind = finding_kind(&tf.finding);
        let key = PrimaryKey {
            file_path: tf.file_path.clone(),
            kind,
        };
        primary_groups.entry(key).or_default().push(tf);
    }

    // Step 2: Within each primary group, form merge clusters based on
    // line-range overlap (secondary key) or shared canonical_pattern.
    let mut all_findings: Vec<Finding> = Vec::new();
    let mut all_suppressed: Vec<Finding> = Vec::new();
    let mut all_decisions: Vec<IntegratorDecisionRecord> = Vec::new();

    for (primary_key, group) in &primary_groups {
        let clusters = form_line_clusters(group);

        for cluster in clusters {
            let (finding, decision) = process_cluster(&cluster, primary_key, config);

            // Write audit record if writer is present.
            if let Some(ref writer) = config.audit_writer
                && let Err(e) = writer.write(&decision)
            {
                tracing::warn!(
                    target: "quorum::skill_integrator",
                    error = %e,
                    "failed to write integrator decision audit record"
                );
            }

            match decision.decision {
                IntegratorDecision::Suppressed => {
                    all_suppressed.push(finding);
                }
                IntegratorDecision::Merged | IntegratorDecision::PassThrough => {
                    all_findings.push(finding);
                }
            }
            all_decisions.push(decision);
        }
    }

    // Step 3: Sort output deterministically.
    // Primary: severity desc, secondary: confidence desc, tertiary: (file_path, line_start).
    // We need file_path for tertiary sort but Finding doesn't carry it.
    // Use the cluster_key from decisions to build a lookup, or sort by
    // finding fields only (line_start as proxy).
    all_findings.sort_by(|a, b| {
        // Severity desc (Critical > High > ... > Info, so reverse).
        b.severity
            .cmp(&a.severity)
            .then_with(|| {
                // Confidence desc.
                let ca = confidence_of(a);
                let cb = confidence_of(b);
                cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                a.line_start
                    .cmp(&b.line_start)
                    .then_with(|| a.line_end.cmp(&b.line_end))
                    .then_with(|| a.title.cmp(&b.title))
            })
    });

    IntegratorOutput {
        findings: all_findings,
        suppressed: all_suppressed,
        decisions: all_decisions,
    }
}

// ---------------------------------------------------------------------------
// Line-range clustering within a primary-key group
// ---------------------------------------------------------------------------

/// Form merge clusters within a primary-key group based on line-range overlap
/// or shared canonical_pattern.
///
/// Uses a greedy single-pass approach: for each finding, try to merge into an
/// existing cluster. A finding joins a cluster if:
/// - It shares an identical `canonical_pattern` with any finding in the cluster
///   (short-circuits line check), OR
/// - Its line range overlaps at least 50% of the shorter range with any finding
///   in the cluster.
///
/// Returns a Vec of clusters, each containing references to the TaggedFindings.
fn form_line_clusters(group: &[TaggedFinding]) -> Vec<Vec<&TaggedFinding>> {
    let mut clusters: Vec<Vec<&TaggedFinding>> = Vec::new();

    for tf in group {
        let mut merged_into = None;

        for (ci, cluster) in clusters.iter().enumerate() {
            if should_merge_into_cluster(tf, cluster) {
                merged_into = Some(ci);
                break;
            }
        }

        if let Some(ci) = merged_into {
            clusters[ci].push(tf);
        } else {
            clusters.push(vec![tf]);
        }
    }

    clusters
}

/// Check if a tagged finding should merge into an existing cluster.
fn should_merge_into_cluster(tf: &TaggedFinding, cluster: &[&TaggedFinding]) -> bool {
    let new_range = (tf.finding.line_start, tf.finding.line_end);
    let new_pattern = tf.finding.canonical_pattern.as_deref();

    for existing in cluster {
        // Short-circuit: identical canonical_pattern.
        if let (Some(np), Some(ep)) = (new_pattern, existing.finding.canonical_pattern.as_deref())
            && !np.is_empty()
            && np == ep
        {
            return true;
        }

        // Line-range overlap check.
        let existing_range = (existing.finding.line_start, existing.finding.line_end);
        if ranges_overlap_enough(new_range, existing_range) {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Process a single cluster
// ---------------------------------------------------------------------------

/// Process a cluster: merge or pass-through, then check suppression.
///
/// Returns `(output_finding, decision_record)`.
fn process_cluster(
    cluster: &[&TaggedFinding],
    primary_key: &PrimaryKey,
    config: &IntegratorConfig,
) -> (Finding, IntegratorDecisionRecord) {
    let is_single = cluster.len() == 1;

    // Compute merged fields.
    let max_severity = cluster
        .iter()
        .map(|tf| &tf.finding.severity)
        .max()
        .cloned()
        .unwrap_or(Severity::Info);

    // Collapse ensemble duplicates before noisy-or.
    let noisy_or_pairs = collapse_ensemble_duplicates(cluster);
    let merged_confidence = noisy_or(&noisy_or_pairs);

    // Pick the highest-confidence finding for the body (description).
    let best = cluster
        .iter()
        .max_by(|a, b| {
            let ca = confidence_of(&a.finding);
            let cb = confidence_of(&b.finding);
            ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap(); // cluster is non-empty

    // Compute originating_skills union, ordered by max confidence desc.
    let mut skill_max: BTreeMap<String, f64> = BTreeMap::new();
    for tf in cluster {
        if let Some(ref skill) = tf.finding.originating_skill
            && !skill.is_empty()
        {
            let conf = confidence_of(&tf.finding);
            let entry = skill_max.entry(skill.clone()).or_insert(0.0);
            if conf > *entry {
                *entry = conf;
            }
        }
    }
    let mut skill_confs: Vec<(String, f64)> = skill_max.into_iter().collect();
    skill_confs.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    let originating_skills: Vec<String> = skill_confs.iter().map(|(s, _)| s.clone()).collect();

    // Build the merged description.
    let mut description = best.finding.description.clone();
    if originating_skills.len() > 1 {
        let trailer = format!(
            "\n\nAlso flagged by: {}",
            originating_skills[1..].join(", ")
        );
        let sanitized_trailer = sanitize_output(&trailer);
        description.push_str(&sanitized_trailer);
    }

    // Compute the bounding line range across the cluster.
    let line_start = cluster
        .iter()
        .map(|tf| tf.finding.line_start)
        .min()
        .unwrap_or(1);
    let line_end = cluster
        .iter()
        .map(|tf| tf.finding.line_end)
        .max()
        .unwrap_or(1);

    // Build the output finding.
    let output_id = new_finding_ulid();
    let mut output = best.finding.clone();
    output.id = output_id.clone();
    output.severity = max_severity.clone();
    output.confidence = Some(merged_confidence as f32);
    output.description = description;
    output.line_start = line_start;
    output.line_end = line_end;

    // Set originating_skill to the first (highest-confidence) skill.
    if let Some(first_skill) = originating_skills.first() {
        output.originating_skill = Some(first_skill.clone());
    }

    // Merge evidence from all findings.
    let mut all_evidence: Vec<String> = Vec::new();
    for tf in cluster {
        for ev in &tf.finding.evidence {
            if !all_evidence.contains(ev) {
                all_evidence.push(ev.clone());
            }
        }
    }
    output.evidence = all_evidence;

    // Check suppression.
    let suppressed = merged_confidence < config.confidence_floor;

    // Determine decision type.
    let decision_type = if suppressed {
        IntegratorDecision::Suppressed
    } else if is_single {
        IntegratorDecision::PassThrough
    } else {
        IntegratorDecision::Merged
    };

    // Build calibrator_weights map (skill -> 1.0 for now; calibrator
    // integration will refine these in a follow-up).
    let calibrator_weights: HashMap<String, f64> = originating_skills
        .iter()
        .map(|s| (s.clone(), 1.0))
        .collect();

    // Build the decision record.
    let severity_label_str = severity_label(&max_severity);
    let reason = match decision_type {
        IntegratorDecision::Merged => {
            format!(
                "{} findings merged from skills: {}",
                cluster.len(),
                originating_skills.join(", ")
            )
        }
        IntegratorDecision::Suppressed => {
            format!(
                "confidence {merged_confidence:.3} below floor {:.3}",
                config.confidence_floor
            )
        }
        IntegratorDecision::PassThrough => "single finding, no merge needed".to_owned(),
    };

    let decision = IntegratorDecisionRecord {
        run_id: config.run_id.clone(),
        ts: Utc::now(),
        decision: decision_type,
        cluster_key: ClusterKey {
            file_path: primary_key.file_path.clone(),
            line_range: (line_start, line_end),
            finding_kind: primary_key.kind.clone(),
        },
        input_finding_ids: cluster.iter().map(|tf| tf.finding.id.clone()).collect(),
        input_confidences: cluster
            .iter()
            .map(|tf| confidence_of(&tf.finding))
            .collect(),
        input_severities: cluster
            .iter()
            .map(|tf| severity_label(&tf.finding.severity))
            .collect(),
        calibrator_weights,
        confidence_floor: config.confidence_floor,
        output_finding_id: if suppressed { None } else { Some(output_id) },
        output_confidence: merged_confidence,
        severity_pre_clamp: severity_label_str.clone(),
        severity_post_clamp: severity_label_str,
        reason,
        originating_skills,
    };

    (output, decision)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Severity, Source};

    // -- Helpers --------------------------------------------------------------

    fn tagged(file_path: &str, finding: Finding) -> TaggedFinding {
        TaggedFinding {
            file_path: file_path.to_owned(),
            finding,
        }
    }

    fn default_config() -> IntegratorConfig {
        IntegratorConfig {
            run_id: "test-run-001".to_owned(),
            confidence_floor: DEFAULT_CONFIDENCE_FLOOR,
            audit_writer: None,
        }
    }

    // =========================================================================
    // Slug normalization (~4)
    // =========================================================================

    #[test]
    fn slug_simple_title() {
        assert_eq!(
            normalize_title_slug("SQL Injection Risk"),
            "sql-injection-risk"
        );
    }

    #[test]
    fn slug_strips_non_ascii() {
        assert_eq!(
            normalize_title_slug("buffer overflow\u{00AE}"),
            "buffer-overflow"
        );
    }

    #[test]
    fn slug_collapses_dashes() {
        assert_eq!(normalize_title_slug("use---after---free"), "use-after-free");
    }

    #[test]
    fn slug_empty_input() {
        assert_eq!(normalize_title_slug(""), "");
    }

    #[test]
    fn slug_strips_vendor_terms() {
        assert_eq!(
            normalize_title_slug("Potential SQL Injection Vulnerability"),
            "sql-injection"
        );
    }

    // =========================================================================
    // Finding kind (~3)
    // =========================================================================

    #[test]
    fn kind_uses_rule_id_when_present() {
        let f = FindingBuilder::new()
            .rule_id("ast-grep:python/bare-except-pass")
            .title("Bare except")
            .build();
        assert_eq!(finding_kind(&f), "ast-grep:python/bare-except-pass");
    }

    #[test]
    fn kind_uses_title_slug_for_llm() {
        let f = FindingBuilder::new()
            .title("Unvalidated User Input")
            .build();
        assert_eq!(finding_kind(&f), "unvalidated-user-input");
    }

    #[test]
    fn kind_empty_title() {
        let f = FindingBuilder::new().title("").build();
        assert_eq!(finding_kind(&f), "");
    }

    #[test]
    fn kind_vendor_only_title_falls_back_to_lowercase() {
        let f = FindingBuilder::new()
            .title("Potential Vulnerability")
            .build();
        assert_eq!(finding_kind(&f), "potential vulnerability");
    }

    // =========================================================================
    // Range overlap (~5)
    // =========================================================================

    #[test]
    fn ranges_identical_overlaps() {
        assert!(ranges_overlap_enough((10, 20), (10, 20)));
    }

    #[test]
    fn ranges_50_percent_overlaps() {
        // Range a: 10-19 (len 10), range b: 15-24 (len 10).
        // Overlap: 15-19 = 5 lines. 5/10 = 50%.
        assert!(ranges_overlap_enough((10, 19), (15, 24)));
    }

    #[test]
    fn ranges_under_50_percent_no_overlap() {
        // Range a: 10-19 (len 10), range b: 16-25 (len 10).
        // Overlap: 16-19 = 4 lines. 4/10 = 40% < 50%.
        assert!(!ranges_overlap_enough((10, 19), (16, 25)));
    }

    #[test]
    fn ranges_no_overlap_at_all() {
        assert!(!ranges_overlap_enough((1, 10), (20, 30)));
    }

    #[test]
    fn ranges_contained_within() {
        // Inner range (12, 15) len 4 is fully inside (10, 20) len 11.
        // Overlap = 4. Shorter = 4. 4/4 = 100%.
        assert!(ranges_overlap_enough((10, 20), (12, 15)));
    }

    #[test]
    fn ranges_single_line_same() {
        assert!(ranges_overlap_enough((5, 5), (5, 5)));
    }

    #[test]
    fn ranges_single_line_different() {
        assert!(!ranges_overlap_enough((5, 5), (6, 6)));
    }

    // =========================================================================
    // Noisy-or (~2)
    // =========================================================================

    #[test]
    fn noisy_or_single_source() {
        let result = noisy_or(&[(0.8, 1.0)]);
        assert!((result - 0.8).abs() < 1e-10);
    }

    #[test]
    fn noisy_or_multiple_sources() {
        // 1 - (1 - 0.8*1.0) * (1 - 0.6*1.0) = 1 - 0.2 * 0.4 = 1 - 0.08 = 0.92
        let result = noisy_or(&[(0.8, 1.0), (0.6, 1.0)]);
        assert!((result - 0.92).abs() < 1e-10);
    }

    #[test]
    fn noisy_or_empty() {
        assert!((noisy_or(&[]) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn noisy_or_clamped_inputs() {
        // Inputs > 1.0 should be clamped.
        let result = noisy_or(&[(1.5, 1.0)]);
        assert!((result - 1.0).abs() < 1e-10);
    }

    // =========================================================================
    // Integration tests (~10)
    // =========================================================================

    #[test]
    fn single_finding_passes_through() {
        let f = FindingBuilder::new()
            .id("F001")
            .title("Test finding")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();

        let config = default_config();
        let output = integrate(vec![tagged("src/main.rs", f)], &config);

        assert_eq!(output.findings.len(), 1);
        assert!(output.suppressed.is_empty());
        assert_eq!(output.decisions.len(), 1);
        assert_eq!(
            output.decisions[0].decision,
            IntegratorDecision::PassThrough
        );
    }

    #[test]
    fn two_findings_same_key_merged() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .originating_skill("security")
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(12, 18)
            .originating_skill("correctness")
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 1, "should merge into one finding");
        let merged = &output.findings[0];
        assert_eq!(merged.severity, Severity::High, "severity = max");
        // Noisy-or of 0.8 and 0.6: 1 - (1-0.8)*(1-0.6) = 0.92
        let conf = confidence_of(merged);
        assert!(
            (conf - 0.92).abs() < 0.01,
            "confidence should be noisy-or; got {conf}"
        );
        // Both skills in originating_skills.
        assert_eq!(output.decisions.len(), 1);
        let decision = &output.decisions[0];
        assert_eq!(decision.decision, IntegratorDecision::Merged);
        assert!(decision.originating_skills.contains(&"security".to_owned()));
        assert!(
            decision
                .originating_skills
                .contains(&"correctness".to_owned())
        );
    }

    #[test]
    fn two_findings_different_files_not_merged() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::High)
            .lines(10, 20)
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/lib.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 2, "different files should not merge");
    }

    #[test]
    fn two_findings_same_file_different_kind_not_merged() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("Buffer Overflow")
            .confidence(0.7)
            .severity(Severity::High)
            .lines(10, 20)
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 2, "different kinds should not merge");
    }

    #[test]
    fn two_findings_same_kind_non_overlapping_ranges_not_merged() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.7)
            .severity(Severity::High)
            .lines(100, 110)
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(
            output.findings.len(),
            2,
            "non-overlapping ranges should not merge"
        );
    }

    #[test]
    fn finding_below_floor_suppressed() {
        let f = FindingBuilder::new()
            .id("F001")
            .title("Minor style")
            .confidence(0.1)
            .severity(Severity::Info)
            .lines(1, 1)
            .build();

        let config = IntegratorConfig {
            confidence_floor: 0.30,
            ..default_config()
        };
        let output = integrate(vec![tagged("src/main.rs", f)], &config);

        assert!(output.findings.is_empty(), "should be suppressed");
        assert_eq!(output.suppressed.len(), 1);
        assert_eq!(output.decisions[0].decision, IntegratorDecision::Suppressed);
    }

    #[test]
    fn ensemble_same_skill_collapsed_before_merge() {
        // 3 findings from "security" on different models should count as 1
        // source in noisy-or (max confidence among them).
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .originating_skill("security")
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.7)
            .severity(Severity::High)
            .lines(10, 20)
            .originating_skill("security")
            .source(Source::Llm("gemini-2.5-pro".into()))
            .build();
        let f3 = FindingBuilder::new()
            .id("F003")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(10, 20)
            .originating_skill("security")
            .source(Source::Llm("claude-4".into()))
            .build();

        let config = default_config();
        let output = integrate(
            vec![
                tagged("src/main.rs", f1),
                tagged("src/main.rs", f2),
                tagged("src/main.rs", f3),
            ],
            &config,
        );

        assert_eq!(output.findings.len(), 1);
        let merged = &output.findings[0];
        // All 3 are from "security" skill: collapsed to 1 source with
        // max confidence 0.8. Noisy-or of single (0.8, 1.0) = 0.8.
        let conf = confidence_of(merged);
        assert!(
            (conf - 0.8).abs() < 0.01,
            "ensemble from same skill should collapse to max; got {conf}"
        );
    }

    #[test]
    fn output_sorted_severity_desc_confidence_desc() {
        let f_info = FindingBuilder::new()
            .id("F001")
            .title("Low prio A")
            .confidence(0.9)
            .severity(Severity::Info)
            .lines(1, 1)
            .build();
        let f_high = FindingBuilder::new()
            .id("F002")
            .title("High prio B")
            .confidence(0.5)
            .severity(Severity::High)
            .lines(10, 10)
            .build();
        let f_critical = FindingBuilder::new()
            .id("F003")
            .title("Critical C")
            .confidence(0.7)
            .severity(Severity::Critical)
            .lines(20, 20)
            .build();

        let config = default_config();
        let output = integrate(
            vec![
                tagged("src/main.rs", f_info),
                tagged("src/main.rs", f_high),
                tagged("src/main.rs", f_critical),
            ],
            &config,
        );

        assert_eq!(output.findings.len(), 3);
        assert_eq!(output.findings[0].severity, Severity::Critical);
        assert_eq!(output.findings[1].severity, Severity::High);
        assert_eq!(output.findings[2].severity, Severity::Info);
    }

    #[test]
    fn determinism_test() {
        // Same input produces identical output across multiple runs.
        // Use fixed IDs and deterministic config.
        let make_input = || {
            vec![
                tagged(
                    "src/main.rs",
                    FindingBuilder::new()
                        .id("DET-001")
                        .title("SQL Injection")
                        .confidence(0.8)
                        .severity(Severity::High)
                        .lines(10, 20)
                        .originating_skill("security")
                        .build(),
                ),
                tagged(
                    "src/main.rs",
                    FindingBuilder::new()
                        .id("DET-002")
                        .title("SQL Injection")
                        .confidence(0.6)
                        .severity(Severity::Medium)
                        .lines(12, 18)
                        .originating_skill("correctness")
                        .build(),
                ),
                tagged(
                    "src/lib.rs",
                    FindingBuilder::new()
                        .id("DET-003")
                        .title("Buffer Overflow")
                        .confidence(0.9)
                        .severity(Severity::Critical)
                        .lines(50, 60)
                        .build(),
                ),
            ]
        };

        let config = default_config();
        let out1 = integrate(make_input(), &config);
        let out2 = integrate(make_input(), &config);

        // Compare everything except IDs (which are fresh ULIDs) and timestamps.
        assert_eq!(out1.findings.len(), out2.findings.len());
        assert_eq!(out1.suppressed.len(), out2.suppressed.len());
        assert_eq!(out1.decisions.len(), out2.decisions.len());

        for (a, b) in out1.findings.iter().zip(out2.findings.iter()) {
            assert_eq!(a.severity, b.severity);
            assert_eq!(a.title, b.title);
            assert_eq!(a.confidence, b.confidence);
            assert_eq!(a.line_start, b.line_start);
            assert_eq!(a.line_end, b.line_end);
        }

        for (a, b) in out1.decisions.iter().zip(out2.decisions.iter()) {
            assert_eq!(a.decision, b.decision);
            assert_eq!(a.cluster_key, b.cluster_key);
            assert_eq!(a.input_finding_ids, b.input_finding_ids);
            assert_eq!(a.output_confidence, b.output_confidence);
        }
    }

    #[test]
    fn audit_records_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("integrator.jsonl");
        let writer = Arc::new(AuditWriter::<IntegratorDecisionRecord>::new(path.clone()));

        let config = IntegratorConfig {
            run_id: "audit-test-run".to_owned(),
            confidence_floor: DEFAULT_CONFIDENCE_FLOOR,
            audit_writer: Some(writer),
        };

        let f = FindingBuilder::new()
            .id("F001")
            .title("Test")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(1, 10)
            .build();

        let output = integrate(vec![tagged("src/main.rs", f)], &config);
        assert_eq!(output.decisions.len(), 1);

        // Verify the file was written.
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.is_empty(), "audit file should have been written");
        assert!(
            content.contains("audit-test-run"),
            "audit record should contain run_id"
        );
    }

    #[test]
    fn empty_input_returns_empty_output() {
        let config = default_config();
        let output = integrate(vec![], &config);
        assert!(output.findings.is_empty());
        assert!(output.suppressed.is_empty());
        assert!(output.decisions.is_empty());
    }

    #[test]
    fn canonical_pattern_short_circuits_line_check() {
        // Two findings with the same canonical_pattern but non-overlapping
        // lines should still merge.
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.7)
            .severity(Severity::High)
            .lines(10, 20)
            .canonical_pattern("sql-injection-concat")
            .originating_skill("security")
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(100, 110)
            .canonical_pattern("sql-injection-concat")
            .originating_skill("correctness")
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(
            output.findings.len(),
            1,
            "identical canonical_pattern should merge despite non-overlapping lines"
        );
        assert_eq!(output.decisions[0].decision, IntegratorDecision::Merged);
    }

    #[test]
    fn default_confidence_used_when_none() {
        let f = FindingBuilder::new()
            .id("F001")
            .title("No confidence")
            .severity(Severity::Medium)
            .lines(1, 1)
            // No .confidence() call => None
            .build();

        let config = default_config();
        let output = integrate(vec![tagged("src/main.rs", f)], &config);

        assert_eq!(output.findings.len(), 1);
        let conf = confidence_of(&output.findings[0]);
        // The merged output should use the noisy-or of 0.5 (default) = 0.5.
        assert!(
            (conf - 0.5).abs() < 0.01,
            "None confidence should default to 0.5; got {conf}"
        );
    }

    #[test]
    fn merged_finding_gets_fresh_ulid() {
        let f1 = FindingBuilder::new()
            .id("ORIGINAL-001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .id("ORIGINAL-002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(12, 18)
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 1);
        let merged_id = &output.findings[0].id;
        assert_ne!(merged_id, "ORIGINAL-001");
        assert_ne!(merged_id, "ORIGINAL-002");
        assert_eq!(merged_id.len(), 26, "should be a ULID");
    }

    #[test]
    fn also_flagged_by_trailer_sanitized() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .description("Found SQL injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .originating_skill("security")
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .description("Also found SQL injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(12, 18)
            .originating_skill("correctness")
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 1);
        let desc = &output.findings[0].description;
        assert!(
            desc.contains("Also flagged by: correctness"),
            "should have trailer; got: {desc}"
        );
    }

    #[test]
    fn evidence_merged_deduped() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .evidence("dataflow: req -> db")
            .evidence("line 15: execute()")
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(12, 18)
            .evidence("dataflow: req -> db") // duplicate
            .evidence("stack: handler -> query")
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 1);
        let evidence = &output.findings[0].evidence;
        assert_eq!(evidence.len(), 3, "duplicates should be deduped");
        assert!(evidence.contains(&"dataflow: req -> db".to_owned()));
        assert!(evidence.contains(&"line 15: execute()".to_owned()));
        assert!(evidence.contains(&"stack: handler -> query".to_owned()));
    }

    #[test]
    fn bounding_line_range_computed() {
        let f1 = FindingBuilder::new()
            .id("F001")
            .title("SQL Injection")
            .confidence(0.8)
            .severity(Severity::High)
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .id("F002")
            .title("SQL Injection")
            .confidence(0.6)
            .severity(Severity::Medium)
            .lines(15, 25)
            .build();

        let config = default_config();
        let output = integrate(
            vec![tagged("src/main.rs", f1), tagged("src/main.rs", f2)],
            &config,
        );

        assert_eq!(output.findings.len(), 1);
        assert_eq!(output.findings[0].line_start, 10);
        assert_eq!(output.findings[0].line_end, 25);
    }
}
