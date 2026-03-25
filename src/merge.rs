use crate::finding::Finding;

pub fn merge_findings(groups: Vec<Vec<Finding>>, similarity_threshold: f64) -> Vec<Finding> {
    let all: Vec<Finding> = groups.into_iter().flatten().collect();
    if all.is_empty() {
        return vec![];
    }

    let mut merged: Vec<Finding> = Vec::new();

    for finding in all {
        let mut found_match = false;
        for existing in merged.iter_mut() {
            if similarity(existing, &finding) >= similarity_threshold {
                // Merge: keep highest severity, widen line range, combine evidence
                if finding.severity > existing.severity {
                    existing.severity = finding.severity.clone();
                }
                existing.line_start = existing.line_start.min(finding.line_start);
                existing.line_end = existing.line_end.max(finding.line_end);
                for e in &finding.evidence {
                    if !existing.evidence.contains(e) {
                        existing.evidence.push(e.clone());
                    }
                }
                found_match = true;
                break;
            }
        }
        if !found_match {
            merged.push(finding);
        }
    }

    // Sort: highest severity first, then by line number
    merged.sort_by(|a, b| {
        b.severity.cmp(&a.severity).then(a.line_start.cmp(&b.line_start))
    });

    merged
}

fn similarity(a: &Finding, b: &Finding) -> f64 {
    let mut score = 0.0;
    let mut weights = 0.0;

    // Title similarity (highest weight)
    let title_sim = string_similarity(&a.title, &b.title);
    score += title_sim * 3.0;
    weights += 3.0;

    // Category match
    if a.category == b.category {
        score += 2.0;
    }
    weights += 2.0;

    // Line range overlap
    let overlap = line_overlap(a.line_start, a.line_end, b.line_start, b.line_end);
    score += overlap * 2.0;
    weights += 2.0;

    score / weights
}

fn string_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    // Jaccard similarity on words
    let words_a: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let words_b: std::collections::HashSet<&str> = b.split_whitespace().collect();

    let intersection = words_a.intersection(&words_b).count() as f64;
    let union = words_a.union(&words_b).count() as f64;

    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn line_overlap(a_start: u32, a_end: u32, b_start: u32, b_end: u32) -> f64 {
    let overlap_start = a_start.max(b_start);
    let overlap_end = a_end.min(b_end);

    if overlap_start > overlap_end {
        return 0.0;
    }

    let overlap_size = (overlap_end - overlap_start + 1) as f64;
    let total_span = (a_start.min(b_start)..=a_end.max(b_end)).count() as f64;

    overlap_size / total_span
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Severity, Source};

    #[test]
    fn merge_identical_findings_deduped() {
        let f1 = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .lines(42, 50)
            .source(Source::Llm("gpt-5.4".into()))
            .build();
        let f2 = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .lines(42, 50)
            .source(Source::Llm("claude".into()))
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn merge_different_findings_preserved() {
        let f1 = FindingBuilder::new()
            .title("SQL injection")
            .category("security")
            .lines(42, 50)
            .build();
        let f2 = FindingBuilder::new()
            .title("Unused import")
            .category("style")
            .lines(1, 1)
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn merge_picks_highest_severity() {
        let f1 = FindingBuilder::new()
            .title("SQL injection")
            .severity(Severity::Medium)
            .lines(42, 50)
            .build();
        let f2 = FindingBuilder::new()
            .title("SQL injection")
            .severity(Severity::Critical)
            .lines(42, 50)
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].severity, Severity::Critical);
    }

    #[test]
    fn merge_empty_input() {
        let result = merge_findings(vec![], 0.8);
        assert!(result.is_empty());
    }

    #[test]
    fn merge_single_source_passthrough() {
        let f1 = FindingBuilder::new().title("Bug 1").lines(10, 20).build();
        let f2 = FindingBuilder::new().title("Bug 2").lines(30, 40).build();
        let result = merge_findings(vec![vec![f1, f2]], 0.8);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn merge_overlapping_line_ranges() {
        let f1 = FindingBuilder::new()
            .title("Insecure pattern")
            .category("security")
            .lines(42, 50)
            .build();
        let f2 = FindingBuilder::new()
            .title("Insecure pattern")
            .category("security")
            .lines(45, 55)
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn merge_non_overlapping_same_title_not_merged() {
        let f1 = FindingBuilder::new()
            .title("Unused variable")
            .lines(10, 10)
            .build();
        let f2 = FindingBuilder::new()
            .title("Unused variable")
            .lines(100, 100)
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn merge_sorted_by_severity_then_line() {
        let f1 = FindingBuilder::new()
            .title("Info finding")
            .severity(Severity::Info)
            .lines(1, 1)
            .build();
        let f2 = FindingBuilder::new()
            .title("Critical finding")
            .severity(Severity::Critical)
            .lines(50, 60)
            .build();
        let f3 = FindingBuilder::new()
            .title("Medium finding")
            .severity(Severity::Medium)
            .lines(20, 30)
            .build();
        let result = merge_findings(vec![vec![f1, f2, f3]], 0.8);
        assert_eq!(result[0].severity, Severity::Critical);
        assert_eq!(result[1].severity, Severity::Medium);
        assert_eq!(result[2].severity, Severity::Info);
    }

    #[test]
    fn merge_idempotent() {
        let f1 = FindingBuilder::new()
            .title("Bug A")
            .lines(10, 20)
            .severity(Severity::High)
            .build();
        let f2 = FindingBuilder::new()
            .title("Bug A")
            .lines(10, 20)
            .severity(Severity::Medium)
            .build();
        let first = merge_findings(vec![vec![f1.clone(), f2.clone()]], 0.8);
        let second = merge_findings(vec![first], 0.8);
        assert_eq!(second.len(), 1);
    }

    #[test]
    fn merge_preserves_evidence_from_merged_findings() {
        let f1 = FindingBuilder::new()
            .title("SQL injection")
            .lines(42, 50)
            .evidence("dataflow analysis")
            .build();
        let f2 = FindingBuilder::new()
            .title("SQL injection")
            .lines(42, 50)
            .evidence("pattern match")
            .build();
        let result = merge_findings(vec![vec![f1], vec![f2]], 0.8);
        assert_eq!(result.len(), 1);
        assert!(result[0].evidence.len() >= 2);
    }

    #[test]
    fn similarity_identical_findings_is_one() {
        let f = FindingBuilder::new()
            .title("Bug")
            .category("security")
            .lines(10, 20)
            .build();
        assert!((similarity(&f, &f) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn similarity_completely_different_is_low() {
        let f1 = FindingBuilder::new()
            .title("SQL injection in auth module")
            .category("security")
            .lines(10, 20)
            .build();
        let f2 = FindingBuilder::new()
            .title("Unused import os")
            .category("style")
            .lines(200, 200)
            .build();
        assert!(similarity(&f1, &f2) < 0.3);
    }
}
