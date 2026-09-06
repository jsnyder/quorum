//! Integration test: `quorum stats --skills` reads the skill invocation audit
//! log (#491). The reader existed and was tested for months with no production
//! caller; these tests pin the caller in place.

use assert_cmd::Command;
use serde_json::Value;
use std::path::Path;
use tempfile::TempDir;

fn quorum(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.env("HOME", home);
    cmd.env_remove("CLAUDE_CODE")
        .env_remove("CODEX_CI")
        .env_remove("GEMINI_CLI")
        .env_remove("AGENT")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("QUORUM_HOME")
        .env_remove("QUORUM_API_KEY");
    cmd
}

fn invocation(skill: &str, findings: u32, seq: u32, parse_error: Option<&str>) -> String {
    let parse = match parse_error {
        Some(c) => format!(r#","parse_error_class":"{c}""#),
        None => String::new(),
    };
    format!(
        r#"{{"skill_run_id":"run-{skill}-{seq}","run_id":"review-1",
"ts":"2026-09-0{day}T12:00:00Z","skill_name":"{skill}","skill_version":"1.0.0",
"manifest_sha256":"{h}","prompt_family":"default","prompt_sha256":"{h}",
"model":"gpt-5.6","model_was_fallback":false,"axis_selection_source":"default",
"capability_mode":"pure","trust_tier":"bundled","file_path":"src/main.rs",
"file_sha256":"{h}","tokens_in":100,"tokens_out":20,"duration_ms":1000,
"findings_emitted":{findings},"exit_status":"ok"{parse}}}"#,
        day = (seq % 9) + 1,
        h = "a".repeat(64),
    )
    .replace('\n', "")
}

fn seed_audit_log(home: &Path, lines: &[String]) {
    let dir = home.join(".quorum");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("skill_invocations.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
}

#[test]
fn skills_view_without_audit_log_is_not_an_error() {
    // A fresh install has no log. That is not a failure.
    let tmp = TempDir::new().unwrap();
    quorum(tmp.path())
        .arg("stats")
        .arg("--skills")
        .arg("--json")
        .assert()
        .code(0);
}

#[test]
fn skills_view_reports_per_skill_rows() {
    let tmp = TempDir::new().unwrap();
    let mut lines: Vec<String> = (0..3).map(|i| invocation("security", 2, i, None)).collect();
    lines.push(invocation("correctness", 0, 5, None));

    seed_audit_log(tmp.path(), &lines);

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--skills")
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "skills");
    assert_eq!(v["meta"]["parsed_ok"], 4);
    assert_eq!(v["meta"]["parse_errors"], 0);

    let rows = v["rows"].as_array().unwrap();
    let sec = rows
        .iter()
        .find(|r| r["skill"] == "security")
        .expect("security row");
    assert_eq!(sec["runs"], 3);
    assert_eq!(sec["findings_emitted"], 6);
    assert_eq!(sec["zero_streak"], 0);
}

#[test]
fn skills_view_surfaces_a_zero_finding_blackout() {
    // The 440-invocation blackout, scaled down: a skill that emits nothing
    // while logging wrong_schema must be impossible to miss.
    let tmp = TempDir::new().unwrap();
    let lines: Vec<String> = (0..8)
        .map(|i| invocation("axis", 0, i, Some("wrong_schema")))
        .collect();
    seed_audit_log(tmp.path(), &lines);

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--skills")
        .arg("--json")
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let row = &v["rows"][0];
    assert_eq!(row["skill"], "axis");
    assert_eq!(row["zero_streak"], 8);
    assert_eq!(row["parse_error_classes"]["wrong_schema"], 8);
}

#[test]
fn skills_view_counts_unparseable_rows() {
    // A corrupt audit log silently returning fewer rows is the same bug one
    // level down -- the parse-error count must be reported, not swallowed.
    let tmp = TempDir::new().unwrap();
    let mut lines = vec![invocation("security", 1, 0, None)];
    lines.push("{ not json".to_string());
    seed_audit_log(tmp.path(), &lines);

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--skills")
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["meta"]["parsed_ok"], 1);
    assert_eq!(v["meta"]["parse_errors"], 1);
}

#[test]
fn skills_view_compact_mode_is_single_line() {
    let tmp = TempDir::new().unwrap();
    seed_audit_log(tmp.path(), &[invocation("security", 1, 0, None)]);

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--skills")
        .arg("--compact")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.trim().lines().count(),
        1,
        "compact is one line: {text}"
    );
    assert!(text.contains("security:runs=1"), "got: {text}");
    assert!(text.contains("parse_errors:0"), "got: {text}");
}

// ─── #491 T4: integrator decision log ───

fn decision(kind: &str, pre: &str, post: &str, reason: &str, seq: u32) -> String {
    format!(
        r#"{{"run_id":"review-1","ts":"2026-09-01T12:00:0{s}Z","decision":"{kind}",
"cluster_key":{{"file_path":"src/main.rs","line_range":[1,2],"finding_kind":"security"}},
"input_finding_ids":["f{seq}"],"input_confidences":[0.8],"input_severities":["{pre}"],
"calibrator_weights":{{}},"confidence_floor":0.3,"output_finding_id":"f{seq}",
"output_confidence":0.7,"severity_pre_clamp":"{pre}","severity_post_clamp":"{post}",
"reason":"{reason}","originating_skills":["security"]}}"#,
        s = seq % 10,
    )
    .replace('\n', "")
}

fn seed_integrator_log(home: &Path, lines: &[String]) {
    let dir = home.join(".quorum");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("integrator_decisions.jsonl"),
        format!("{}\n", lines.join("\n")),
    )
    .unwrap();
}

#[test]
fn integrator_view_without_audit_log_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    quorum(tmp.path())
        .arg("stats")
        .arg("--integrator")
        .arg("--json")
        .assert()
        .code(0);
}

#[test]
fn integrator_view_reports_decisions_and_severity_transitions() {
    let tmp = TempDir::new().unwrap();
    let mut lines: Vec<String> = (0..4)
        .map(|i| decision("merged", "high", "medium", "clamped", i))
        .collect();
    lines.push(decision("suppressed", "low", "low", "below floor", 9));
    seed_integrator_log(tmp.path(), &lines);

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--integrator")
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "integrator");
    assert_eq!(v["meta"]["parsed_ok"], 5);
    assert_eq!(v["severity_transitions"]["high->medium"], 4);

    let merged = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["decision"] == "merged")
        .expect("merged row");
    assert_eq!(merged["count"], 4);
    assert_eq!(merged["severity_changed"], 4);
}

#[test]
fn integrator_view_compact_mode_is_single_line() {
    let tmp = TempDir::new().unwrap();
    seed_integrator_log(
        tmp.path(),
        &[decision("suppressed", "high", "high", "dup", 0)],
    );

    let out = quorum(tmp.path())
        .arg("stats")
        .arg("--integrator")
        .arg("--compact")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.trim().lines().count(),
        1,
        "compact is one line: {text}"
    );
    assert!(text.contains("suppressed=1"), "got: {text}");
}
