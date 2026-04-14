use assert_cmd::Command;

#[test]
fn parallel_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.rs");
    std::fs::write(&path, "fn main() { let x = 1; }\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("4")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_1_sequential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.py");
    std::fs::write(&path, "x = 1\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("1")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_0_unlimited() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();

    Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("0")
        .arg(&path)
        .assert()
        .success();
}

#[test]
fn parallel_json_output_valid() {
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=3 {
        let path = dir.path().join(format!("file{}.py", i));
        std::fs::write(&path, format!("x = {}\n", i)).unwrap();
    }

    let output = Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("2")
        .arg("--json")
        .arg(dir.path().join("file1.py"))
        .arg(dir.path().join("file2.py"))
        .arg(dir.path().join("file3.py"))
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() && stdout.trim() != "[]" {
        let _: serde_json::Value = serde_json::from_str(&stdout)
            .expect("parallel JSON output should be valid");
    }
}

#[test]
fn parallel_handles_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.rs");
    std::fs::write(&good, "fn main() {}\n").unwrap();
    let bad = dir.path().join("nonexistent.rs");

    let output = Command::cargo_bin("quorum").unwrap()
        .arg("review")
        .arg("--parallel").arg("2")
        .arg(&good)
        .arg(&bad)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("nonexistent") || stderr.contains("Error"),
        "should report missing file, got: {}", stderr);
}

#[test]
fn parallel_multiple_files_local_only() {
    // Test parallel review of multiple files without API key (local AST only)
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=5 {
        let path = dir.path().join(format!("mod{}.rs", i));
        std::fs::write(&path, format!("fn func{}() {{ let x = {}; }}\n", i, i)).unwrap();
    }

    let mut cmd = Command::cargo_bin("quorum").unwrap();
    cmd.arg("review").arg("--parallel").arg("3");
    for i in 1..=5 {
        cmd.arg(dir.path().join(format!("mod{}.rs", i)));
    }
    cmd.assert().success();
}
