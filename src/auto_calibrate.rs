/// Auto-calibration: a second LLM pass that triages findings and records verdicts.
/// Runs after the initial review to automatically build the feedback corpus.

use crate::feedback::{FeedbackEntry, FeedbackStore, Verdict};
use crate::finding::Finding;
use crate::pipeline::LlmReviewer;

/// Result of auto-calibration on a set of findings.
#[derive(Debug)]
pub struct AutoCalibrationResult {
    pub verdicts: Vec<(Finding, Verdict, String)>, // (finding, verdict, reason)
    pub recorded: usize,
}

/// Ask an LLM to triage findings and record verdicts to the feedback store.
pub fn auto_calibrate(
    findings: &[Finding],
    code: &str,
    file_path: &str,
    reviewer: &dyn LlmReviewer,
    model: &str,
    feedback_store: &FeedbackStore,
) -> anyhow::Result<AutoCalibrationResult> {
    if findings.is_empty() {
        return Ok(AutoCalibrationResult { verdicts: vec![], recorded: 0 });
    }

    let prompt = build_triage_prompt(findings, code, file_path);
    let resp = reviewer.review(&prompt, model)?;
    let verdicts = parse_triage_response(&resp.content, findings)?;

    let mut recorded = 0;
    for (finding, verdict, reason) in &verdicts {
        let entry = FeedbackEntry {
            file_path: file_path.to_string(),
            finding_title: finding.title.clone(),
            finding_category: finding.category.clone(),
            verdict: verdict.clone(),
            reason: reason.clone(),
            model: Some(format!("auto-calibrate:{}", model)),
            timestamp: chrono::Utc::now(),
            provenance: crate::feedback::Provenance::AutoCalibrate(model.to_string()),
        };
        if feedback_store.record(&entry).is_ok() {
            recorded += 1;
        }
    }

    Ok(AutoCalibrationResult { verdicts, recorded })
}

fn build_triage_prompt(findings: &[Finding], code: &str, file_path: &str) -> String {
    let mut prompt = format!(
        "You are a code review calibrator. Given the code from `{}` and a list of review findings, \
         assess each finding's accuracy.\n\n\
         For each finding, respond with a JSON array of objects with:\n\
         - \"index\" (0-based finding number)\n\
         - \"verdict\": \"tp\" (true positive, real issue), \"fp\" (false positive, not real), \
           \"partial\" (real but overstated), or \"wontfix\" (real but not worth fixing)\n\
         - \"reason\": brief explanation\n\n\
         Respond ONLY with the JSON array.\n\n## Code\n```\n",
        file_path
    );
    // Truncate code to avoid huge prompts
    let code_preview: String = code.chars().take(4000).collect();
    prompt.push_str(&code_preview);
    if code.len() > 4000 {
        prompt.push_str("\n... (truncated)");
    }
    prompt.push_str("\n```\n\n## Findings to triage\n");

    for (i, f) in findings.iter().enumerate() {
        let src = match &f.source {
            crate::finding::Source::LocalAst => "local-ast".to_string(),
            crate::finding::Source::Linter(n) => format!("linter:{}", n),
            crate::finding::Source::Llm(n) => format!("llm:{}", n),
        };
        prompt.push_str(&format!(
            "\n{}. [{}] {} (L{}-{}, source: {})\n   {}\n",
            i, f.severity_label(), f.title, f.line_start, f.line_end, src, f.description
        ));
    }

    prompt
}

#[derive(serde::Deserialize)]
struct TriageEntry {
    index: usize,
    verdict: String,
    reason: String,
}

fn parse_triage_response(
    response: &str,
    findings: &[Finding],
) -> anyhow::Result<Vec<(Finding, Verdict, String)>> {
    // Reuse our existing robust JSON extraction
    let cleaned = crate::review::extract_json_array_public(response);
    let entries: Vec<TriageEntry> = serde_json::from_str(&cleaned)?;

    let mut verdicts = Vec::new();
    for entry in entries {
        if entry.index >= findings.len() {
            continue;
        }
        let verdict = match entry.verdict.to_lowercase().as_str() {
            "tp" => Verdict::Tp,
            "fp" => Verdict::Fp,
            "partial" => Verdict::Partial,
            "wontfix" => Verdict::Wontfix,
            _ => continue,
        };
        verdicts.push((findings[entry.index].clone(), verdict, entry.reason));
    }
    Ok(verdicts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{FindingBuilder, Severity};

    #[test]
    fn build_triage_prompt_includes_findings() {
        let findings = vec![
            FindingBuilder::new()
                .title("SQL injection")
                .description("f-string in execute()")
                .severity(Severity::Critical)
                .category("security")
                .lines(10, 10)
                .build(),
        ];
        let prompt = build_triage_prompt(&findings, "cursor.execute(f\"SELECT {x}\")", "app.py");
        assert!(prompt.contains("SQL injection"));
        assert!(prompt.contains("app.py"));
        assert!(prompt.contains("cursor.execute"));
    }

    #[test]
    fn parse_triage_valid_response() {
        let findings = vec![
            FindingBuilder::new().title("Bug A").build(),
            FindingBuilder::new().title("Bug B").build(),
        ];
        let response = r#"[{"index":0,"verdict":"tp","reason":"real bug"},{"index":1,"verdict":"fp","reason":"not an issue"}]"#;
        let verdicts = parse_triage_response(response, &findings).unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0].1, Verdict::Tp);
        assert_eq!(verdicts[1].1, Verdict::Fp);
    }

    #[test]
    fn parse_triage_skips_invalid_index() {
        let findings = vec![FindingBuilder::new().title("Bug").build()];
        let response = r#"[{"index":0,"verdict":"tp","reason":"ok"},{"index":99,"verdict":"fp","reason":"bad index"}]"#;
        let verdicts = parse_triage_response(response, &findings).unwrap();
        assert_eq!(verdicts.len(), 1);
    }

    #[test]
    fn parse_triage_skips_invalid_verdict() {
        let findings = vec![FindingBuilder::new().title("Bug").build()];
        let response = r#"[{"index":0,"verdict":"banana","reason":"invalid"}]"#;
        let verdicts = parse_triage_response(response, &findings).unwrap();
        assert_eq!(verdicts.len(), 0);
    }

    #[test]
    fn auto_calibrate_empty_findings() {
        struct FakeLlm;
        impl LlmReviewer for FakeLlm {
            fn review(&self, _: &str, _: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
                Ok(crate::llm_client::LlmResponse { content: "[]".into(), usage: None })
            }
        }
        let store = FeedbackStore::new(std::path::PathBuf::from("/tmp/auto-cal-test.jsonl"));
        let result = auto_calibrate(&[], "code", "file.rs", &FakeLlm, "test", &store).unwrap();
        assert_eq!(result.recorded, 0);
    }

    #[test]
    fn auto_calibrate_records_verdicts() {
        struct FakeLlm;
        impl LlmReviewer for FakeLlm {
            fn review(&self, _: &str, _: &str) -> anyhow::Result<crate::llm_client::LlmResponse> {
                Ok(crate::llm_client::LlmResponse {
                    content: r#"[{"index":0,"verdict":"tp","reason":"confirmed real"}]"#.into(),
                    usage: None,
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let store = FeedbackStore::new(dir.path().join("feedback.jsonl"));
        let findings = vec![FindingBuilder::new().title("SQL injection").build()];
        let result = auto_calibrate(&findings, "code", "app.py", &FakeLlm, "test", &store).unwrap();
        assert_eq!(result.recorded, 1);
        assert_eq!(store.count().unwrap(), 1);
    }
}
