//! Issue #592: a finding's file path reaches the GitHub posting path.
//!
//! `Finding` has no file field. In `review --json` the path lives on the
//! enclosing group, and posting is the only consumer that needs it back, so
//! `run_review` maintains a second list (`all_review_findings`) that pairs each
//! finding with its file.
//!
//! Two parallel lists are a drift hazard, and drift is exactly how this broke
//! the first time: `post_review` recovered the path from `evidence[0]` -- the
//! matched source text -- behind a comment claiming the pipeline populated it.
//! Every inline comment was classified against a file like
//! `cyclomatic_complexity=21`, matched no diff range, and fell through to the
//! summary body, so inline comments had never worked in any released version.
//!
//! `record_findings` is the single writer. This guard fails if a new site
//! appends to `all_findings` directly, because such a site adds a finding with
//! no path and silently sends it to the body.
//!
//! Scope: it scans source text, so it catches the ordinary mistake rather than
//! a determined evasion -- the same ceiling as `tests/no_env_mutation.rs`,
//! `tests/spawn_helper_guard.rs` and `tests/no_raw_model_output_in_logs.rs`,
//! and acceptable for the same reason.

#[test]
fn findings_are_recorded_only_through_the_single_writer() {
    let main_rs = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("read src/main.rs");

    // `run_review` owns the paired lists. The daemon path (`run_review_via_daemon`)
    // has its own local `all_findings` and does not post to GitHub, so it is not
    // in scope -- bound the scan to the function that is.
    let start = main_rs
        .find("let mut all_review_findings")
        .expect("run_review must still declare the path-carrying list");
    let end = main_rs[start..]
        .find("\nasync fn ")
        .map(|o| start + o)
        .unwrap_or(main_rs.len());
    let body = &main_rs[start..end];

    let offenders: Vec<_> = body
        .lines()
        .enumerate()
        .filter(|(_, l)| {
            let l = l.trim();
            (l.starts_with("all_findings.extend") || l.starts_with("all_findings.push"))
                && !l.contains("record_findings")
        })
        .map(|(i, l)| format!("  +{i}: {}", l.trim()))
        .collect();

    assert!(
        offenders.is_empty(),
        "Findings must be recorded through `record_findings` so the file path \
         travels with them (#592).\n\n\
         Appending to `all_findings` directly adds a finding that the GitHub \
         posting path cannot place, and it will be silently routed to the \
         summary body instead of appearing inline.\n\n\
         Offending sites (offsets within run_review):\n{}",
        offenders.join("\n")
    );

    // The guard is only meaningful while the writer exists under that name.
    assert!(
        body.contains("record_findings("),
        "no call to `record_findings` in run_review -- this guard has stopped \
         guarding anything"
    );
}
