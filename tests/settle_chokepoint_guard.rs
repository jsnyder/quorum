//! #639: a file's findings are settled -- hidden outside the diff, then
//! run through the project's suppression rules, both sets counted -- in one
//! place, `settle_file_findings`. Four paths reach it (sequential and
//! parallel, deep and not); before, two did the hiding, one did not count
//! its suppressions, and the deep paths did neither. This fails when a
//! fifth path hand-rolls the sequence again.

#[test]
fn hide_out_of_diff_is_called_only_from_the_settle_helper() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .unwrap();
    let calls: Vec<usize> = src
        .match_indices("pipeline::hide_out_of_diff(")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        calls.len(),
        1,
        "hide_out_of_diff is called from {} sites in src/main.rs; settle in settle_file_findings",
        calls.len()
    );
    let helper = src
        .find("fn settle_file_findings(")
        .expect("settle_file_findings exists");
    let next_fn = src[helper + 1..]
        .find("\nfn ")
        .map(|o| helper + 1 + o)
        .unwrap_or(src.len());
    assert!(
        calls[0] > helper && calls[0] < next_fn,
        "the one hide_out_of_diff call is outside settle_file_findings"
    );
    let suppress_calls = src.matches("suppress::apply_suppressions(").count();
    assert_eq!(
        suppress_calls, 2,
        "apply_suppressions has {suppress_calls} call sites in src/main.rs; both belong in settle_file_findings"
    );
}
