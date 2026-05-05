use std::io::Write;

/// Integration test for `quorum calibrate --backfill-paths --dry-run`.
#[test]
fn calibrate_backfill_paths_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let fb_path = dir.path().join("feedback.jsonl");
    let tr_path = dir.path().join("calibrator_traces.jsonl");

    // Write one feedback entry
    std::fs::write(
        &fb_path,
        "{\"finding_title\":\"A\",\"file_path\":\"f.rs\",\"verdict\":\"tp\"}\n",
    )
    .unwrap();

    // Write one trace without file_path
    std::fs::write(
        &tr_path,
        "{\"finding_title\":\"A\",\"finding_category\":\"c\",\"tp_weight\":1.0,\"fp_weight\":0.0}\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_quorum"))
        .env("QUORUM_HOME", dir.path())
        .args(["calibrate", "--backfill-paths", "--dry-run"])
        .output()
        .unwrap();

    assert!(output.status.success(), "should exit 0, stderr: {}", String::from_utf8_lossy(&output.stderr));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("total backfilled:"),
        "should report stats, got: {stderr}"
    );
    assert!(
        stderr.contains("dry run"),
        "should say dry run, got: {stderr}"
    );

    // Traces file should be unchanged (dry run)
    let content = std::fs::read_to_string(&tr_path).unwrap();
    assert!(
        !content.contains("f.rs"),
        "dry run should not modify file, got: {content}"
    );
}

/// Integration test for actual backfill (writes file).
#[test]
fn calibrate_backfill_paths_writes_file() {
    let dir = tempfile::tempdir().unwrap();
    let fb_path = dir.path().join("feedback.jsonl");
    let tr_path = dir.path().join("calibrator_traces.jsonl");

    std::fs::write(
        &fb_path,
        "{\"finding_title\":\"B\",\"file_path\":\"src/lib.rs\",\"verdict\":\"tp\"}\n",
    )
    .unwrap();

    std::fs::write(
        &tr_path,
        "{\"finding_title\":\"B\",\"finding_category\":\"x\",\"tp_weight\":1.0,\"fp_weight\":0.0}\n",
    )
    .unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_quorum"))
        .env("QUORUM_HOME", dir.path())
        .args(["calibrate", "--backfill-paths"])
        .output()
        .unwrap();

    assert!(output.status.success(), "should exit 0");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("total backfilled:"), "stats: {stderr}");
    assert!(stderr.contains("Backup:"), "backup msg: {stderr}");
    assert!(stderr.contains("Wrote"), "wrote msg: {stderr}");

    // Trace file should now contain file_path
    let content = std::fs::read_to_string(&tr_path).unwrap();
    assert!(
        content.contains("src/lib.rs"),
        "trace should have file_path backfilled, got: {content}"
    );

    // Backup should exist
    let bak_path = dir.path().join("calibrator_traces.jsonl.bak");
    assert!(bak_path.exists(), "backup file should exist");
    let bak_content = std::fs::read_to_string(&bak_path).unwrap();
    assert!(
        !bak_content.contains("src/lib.rs"),
        "backup should be the original without file_path"
    );
}
