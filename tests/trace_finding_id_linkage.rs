//! Integration test for #459: the `finding_id` written into
//! `calibrator_traces.jsonl` must be the *same* id the review emits in
//! `--json`.
//!
//! The unit tests in `calibrator.rs` assert the in-process property (the trace
//! field IS `Finding.id`). This one closes the serialization gap between them:
//! it runs the real binary, reads both artifacts off disk, and compares. A
//! tier-0 join is exact equality, so an id that diverges anywhere along that
//! chain would mis-join silently rather than fail loudly -- which is the
//! failure mode the issue's risk section calls out.
//!
//! No LLM: an AST-only review over a file with a detectable pattern produces
//! findings, and a seeded feedback store makes the calibrator run and write
//! traces.

mod support;

use serde_json::Value;
use std::collections::HashSet;
use support::quorum;
use tempfile::TempDir;

/// Seeded so the calibrator has a corpus and therefore emits traces.
fn seed_feedback(home: &std::path::Path) {
    let qdir = home.join(".quorum");
    std::fs::create_dir_all(&qdir).unwrap();
    let entry = serde_json::json!({
        "file_path": "src/seed.rs",
        "finding_title": "unwrap on a Result can panic",
        "finding_category": "correctness",
        "verdict": "fp",
        "reason": "seeded so calibration runs",
        "timestamp": "2026-01-01T00:00:00Z",
        "provenance": "human",
    });
    std::fs::write(qdir.join("feedback.jsonl"), format!("{entry}\n")).unwrap();
}

fn collect_ids(v: &Value, out: &mut HashSet<String>) {
    match v {
        Value::Array(a) => a.iter().for_each(|i| collect_ids(i, out)),
        Value::Object(o) => {
            if let Some(Value::Array(findings)) = o.get("findings") {
                for f in findings {
                    if let Some(Value::String(id)) = f.get("id") {
                        out.insert(id.clone());
                    }
                }
            }
            o.values().for_each(|i| collect_ids(i, out));
        }
        _ => {}
    }
}

#[test]
fn trace_finding_id_matches_the_json_finding_id_for_the_same_run() {
    let home = TempDir::new().unwrap();
    seed_feedback(home.path());

    let project = TempDir::new().unwrap();
    std::fs::write(project.path().join("Cargo.toml"), "[package]\nname=\"t\"\n").unwrap();
    let src = project.path().join("lib.rs");
    // Several unwrap sites: the Rust AST analyzer flags these with no LLM.
    std::fs::write(
        &src,
        "pub fn a(x: Option<u32>) -> u32 { x.unwrap() }\n\
         pub fn b(y: Option<u32>) -> u32 { y.unwrap() }\n\
         pub fn c(z: Option<u32>) -> u32 { z.unwrap() }\n",
    )
    .unwrap();

    // Exactly one invocation. An earlier draft retried with a different
    // argument shape when the first parse failed, which was a real bug the
    // review caught: the first run can write traces and *then* produce
    // unparseable stdout, after which the trace file holds ids from two runs
    // while only the second run's ids are compared -- a nondeterministic
    // failure. One run, no fallback, so the trace file has exactly one
    // review's worth of ids in it.
    let out = quorum(home.path())
        .args(["review", src.to_str().unwrap(), "--json"])
        .output()
        .expect("review runs");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("review --json must emit JSON ({e}); got: {stdout}"));

    let mut json_ids = HashSet::new();
    collect_ids(&parsed, &mut json_ids);
    assert!(
        !json_ids.is_empty(),
        "review must emit at least one finding with an id; got {stdout}"
    );

    let trace_path = home.path().join(".quorum").join("calibrator_traces.jsonl");
    let traces = std::fs::read_to_string(&trace_path)
        .unwrap_or_else(|e| panic!("calibrator traces must be written at {trace_path:?}: {e}"));

    let mut trace_ids = Vec::new();
    for line in traces.lines().filter(|l| !l.trim().is_empty()) {
        let t: Value = serde_json::from_str(line).expect("trace line must be JSON");
        match t.get("finding_id") {
            Some(Value::String(id)) => trace_ids.push(id.clone()),
            other => panic!("every new trace must carry a finding_id, got {other:?}"),
        }
    }
    assert!(!trace_ids.is_empty(), "expected at least one trace line");

    // The actual assertion: every id the calibrator recorded is an id the
    // review reported. Set membership, not "both are non-empty" -- a trace
    // carrying a freshly minted ULID would satisfy the weaker check and still
    // join nothing.
    for id in &trace_ids {
        assert!(
            json_ids.contains(id),
            "trace finding_id {id} is not among the ids emitted in --json ({json_ids:?}); \
             the id is not single-sourced from Finding.id"
        );
    }
}
