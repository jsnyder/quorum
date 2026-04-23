// Fixture: builder-unwrap-or-default
//
// Pattern from issue #66: reqwest::Client::builder() with timeouts, then
// .build().unwrap_or_default() silently dropped the configuration on
// builder failure.

fn build_client_buggy() {
    // match: configured builder, error silently dropped, default-constructed
    // value is used instead of the configured one.
    let _client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();
}

fn build_client_buggy_chained() {
    // match: even single-line form
    let _ = SomeBuilder::new().build().unwrap_or_default();
}

fn build_client_safe_propagated() -> anyhow::Result<()> {
    // no-match: proper error propagation via `?`
    let _client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    Ok(())
}

fn build_client_safe_expect() {
    // no-match: explicit expect — failure is loud, not silent
    let _client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("reqwest builder is infallible for this config");
}
