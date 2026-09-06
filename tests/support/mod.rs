//! Shared spawn helper for integration tests (#501).
//!
//! # Deny-by-default
//!
//! No test spends money unless it explicitly asks. That holds in every
//! environment and for any combination of env vars the developer happens to
//! have exported: a present `QUORUM_API_KEY` must never be sufficient to make
//! a paid call. Before this module, it was -- see #501 and #23.
//!
//! Three layers enforce it, so that no single mistake is enough:
//!
//! 1. **The env surface is stripped here**, in the constructor, rather than by
//!    each test remembering to call `.env_remove`. Remembering is the
//!    discipline that already failed in five files.
//! 2. **A tripwire base_url** (`TRIPWIRE_BASE_URL`) is set on every spawn with
//!    the private-IP allowance deliberately withheld, so a key that gets in
//!    anyway dies loudly in `validate_base_url` before a packet leaves the
//!    process. See the constant's docs for why that works.
//! 3. **`tests/spawn_helper_guard.rs`** fails if any integration test reaches
//!    the binary without going through this module.
//!
//! Real endpoints are reachable two ways, both explicit: `with_cassette`
//! (replays a recorded response off a local mock -- free) and `quorum_live`
//! (requires `QUORUM_TEST_LIVE=1` -- spends real money).

// Each test file uses a different subset of these helpers.
#![allow(dead_code)]

use assert_cmd::Command;
use std::path::Path;

/// Every env var that can send a request off this machine.
///
/// Stripped on every spawn. `QUORUM_API_KEY` alone would stop the spending,
/// but quorum has four independent outbound paths and only one of them is the
/// LLM client -- an audit for #501 found the others reachable purely from a
/// developer's ambient environment:
///
/// | path | destination | gated on |
/// |---|---|---|
/// | LLM client (src/llm_client.rs) | `QUORUM_BASE_URL` | `QUORUM_API_KEY` |
/// | Context7 (src/context_enrichment.rs:691) | context7.com | `CONTEXT7_API_KEY` or `~/.context7_key` |
/// | registry popularity (src/enrichment_policy.rs:171) | crates.io, npm, PyPI | `QUORUM_CONTEXT7_LIVE_REGISTRY` or `--live-registry` |
/// | GitHub PR post (src/main.rs:1001, :3347) | api.github.com | `GITHUB_TOKEN` + an explicit subcommand |
///
/// Only the first is covered by [`TRIPWIRE_BASE_URL`]; the rest build their
/// own `reqwest::Client` against a hardcoded host and never see
/// `validate_base_url`. They are closed here, at layer 1, by removing the
/// credential that unlocks them. Pinning `HOME` to a tempdir additionally
/// covers Context7's `~/.context7_key` fallback.
const NETWORK_ENV_VARS: &[&str] = &[
    "QUORUM_API_KEY",
    "QUORUM_BASE_URL",
    "QUORUM_MODEL",
    "QUORUM_ENSEMBLE_MODELS",
    "QUORUM_BYPASS_PROXY_CACHE",
    // The judge is a second LLM call on top of the review.
    "QUORUM_JUDGE",
    "QUORUM_JUDGE_MODEL",
    // Escape hatches for the base_url validator. Cleared so a developer who
    // set them for local Ollama work cannot accidentally disarm the tripwire
    // described on TRIPWIRE_BASE_URL.
    "QUORUM_ALLOW_PRIVATE_BASE_URL",
    "QUORUM_UNSAFE_BASE_URL",
    "QUORUM_ALLOWED_BASE_URL_HOSTS",
    // Non-LLM outbound paths, per the table above.
    "CONTEXT7_API_KEY",
    "QUORUM_CONTEXT7_LIVE_REGISTRY",
    "GITHUB_TOKEN",
    "GITHUB_REPOSITORY",
];

/// Env vars that change review behaviour without costing money.
///
/// Not a spending problem -- a hermeticity one, and the same class of bug as
/// #23: a developer who exported one of these while debugging gets different
/// test results than CI, for reasons no test output explains.
const DETERMINISM_ENV_VARS: &[&str] = &[
    "QUORUM_DISABLE_AST_GROUNDING",
    "QUORUM_DISABLE_CALIBRATOR",
    "QUORUM_DISABLE_FEW_SHOT",
    "QUORUM_DISABLE_FUZZY_MATCHING",
    "QUORUM_FORCE_THRESHOLD",
    "QUORUM_NO_RRF",
    "QUORUM_REASONING_EFFORT",
    "QUORUM_RUBRIC_GATE",
    "QUORUM_TRACE",
    "QUORUM_ALLOWED_AGENTS",
];

/// Env vars that `invoked_from` auto-detection keys on.
///
/// Not a money problem -- a hermeticity one. Left set, the same test records a
/// different `invoked_from` depending on whether it ran under Claude Code, CI,
/// or a bare shell.
const CALLER_ENV_VARS: &[&str] = &[
    "CLAUDE_CODE",
    "CODEX_CI",
    "GEMINI_CLI",
    "AGENT",
    "GITHUB_ACTIONS",
];

/// A base_url that cannot reach anything, and that fails *loudly* if used.
///
/// This is layer 2 of the deny-by-default stack. It exploits an asymmetry
/// between quorum's two URL validators:
///
/// - `Config::validate_url` (src/config.rs) permits plaintext http on
///   loopback unconditionally, so setting this never breaks a normal
///   AST-only run.
/// - `validate_base_url` (src/llm_client.rs) does not. A loopback host sets
///   `host_is_private = true`, and with `QUORUM_ALLOW_PRIVATE_BASE_URL`
///   absent -- which `NETWORK_ENV_VARS` guarantees -- it bails immediately with
///   the actionable private-IP error.
///
/// `validate_base_url` runs inside `OpenAiClient::new`, which is only reached
/// when something actually tries to build an LLM client. So the tripwire is
/// inert for the AST-only path and fires only on an un-opted-in LLM call --
/// as a hard error with a readable message, before any network I/O. A silent
/// charge becomes a loud failure.
///
/// Port 9 (discard) is conventional for "nothing is listening here". The port
/// is never dialed; validation rejects the URL first.
pub const TRIPWIRE_BASE_URL: &str = "http://127.0.0.1:9";

/// Set by a developer who has decided to spend real money on a test run.
const LIVE_OPT_IN: &str = "QUORUM_TEST_LIVE";

/// Every var stripped on every spawn, in one iterator.
pub fn stripped_vars() -> impl Iterator<Item = &'static str> {
    NETWORK_ENV_VARS
        .iter()
        .chain(DETERMINISM_ENV_VARS)
        .chain(CALLER_ENV_VARS)
        .copied()
        .chain(std::iter::once("QUORUM_HOME"))
}

/// Strip the ambient environment down to something reproducible.
///
/// Shared by every constructor below so the deny-by-default rules live in
/// exactly one place.
fn sanitize(cmd: &mut Command) {
    for var in stripped_vars() {
        cmd.env_remove(var);
    }
    cmd.env("QUORUM_BASE_URL", TRIPWIRE_BASE_URL);
}

/// The sanctioned way to spawn `quorum` in an integration test.
///
/// Isolates the home directory to `home` and removes every env var that could
/// reach an LLM. Sets both `HOME` (Unix) and `USERPROFILE` (Windows, preferred
/// by `ProdDeps::from_env` per src/context/cli.rs) so a test cannot leak into
/// the developer's real profile on either platform.
pub fn quorum(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").expect("quorum binary builds");
    cmd.env("HOME", home).env("USERPROFILE", home);
    sanitize(&mut cmd);
    cmd
}

/// Same guarantees as [`quorum`], but isolates via `QUORUM_HOME` instead of
/// `HOME`. For tests that exercise the `QUORUM_HOME` override itself.
pub fn quorum_with_quorum_home(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("quorum").expect("quorum binary builds");
    sanitize(&mut cmd);
    cmd.env("QUORUM_HOME", home); // after sanitize, which removes it
    cmd
}

/// `sanitize` for the `std::process::Command` variants.
fn sanitize_std(cmd: &mut std::process::Command) {
    for var in stripped_vars() {
        cmd.env_remove(var);
    }
    cmd.env("QUORUM_BASE_URL", TRIPWIRE_BASE_URL);
}

fn quorum_std_bare() -> std::process::Command {
    std::process::Command::new(assert_cmd::cargo::cargo_bin("quorum"))
}

/// Same guarantees as [`quorum`], as a `std::process::Command`.
///
/// For tests that need to configure stdio (pipes, closed descriptors) or to
/// `spawn` rather than wait -- `assert_cmd::Command` exposes neither.
pub fn quorum_std(home: &Path) -> std::process::Command {
    let mut cmd = quorum_std_bare();
    cmd.env("HOME", home).env("USERPROFILE", home);
    sanitize_std(&mut cmd);
    cmd
}

/// [`quorum_std`] isolated via `QUORUM_HOME` instead of `HOME`.
pub fn quorum_std_with_quorum_home(home: &Path) -> std::process::Command {
    let mut cmd = quorum_std_bare();
    sanitize_std(&mut cmd);
    cmd.env("QUORUM_HOME", home); // after sanitize, which removes it
    cmd
}

/// Replay a recorded LLM response to the spawned binary. Costs nothing.
///
/// Pure env-stripping makes the suite free and hermetic but leaves the LLM
/// path with *zero* integration coverage, which is its own hole. This closes
/// it: a checked-in provider response from `tests/fixtures/llm/<cassette>.json`
/// is served off a loopback `wiremock` server, and the binary reviews against
/// it exactly as it would against a real endpoint.
///
/// The three interacting env vars this needs are the reason the helper exists.
/// `Config::validate_url` accepts loopback http, but `validate_base_url`
/// rejects it unless `QUORUM_ALLOW_PRIVATE_BASE_URL=1` -- deliberate post-#167
/// semantics, since the scheme check keys on whether the host is actually
/// private rather than on the env var. Getting one of the three wrong yields a
/// confusing failure, so no individual test should have to.
///
/// The closure form keeps the server and its runtime alive for exactly the
/// duration of the run, and shuts both down after.
///
/// # Returns the requests the binary actually sent
///
/// The mock matches any `POST /chat/completions` and does not inspect the
/// request body, so on its own it would replay the cassette no matter what
/// the binary sent -- including a truncated prompt, a missing system message,
/// or no prompt at all. That is the classic VCR staleness hole: the test stays
/// green while testing nothing about prompt assembly.
///
/// Matching strictly on the body would be the over-correction, since then
/// every prompt tweak forces a re-record with real money. Instead the captured
/// request bodies come back with the result, so a test can pin a few
/// invariants that break loudly on a real regression and survive a wording
/// change. See `tests/llm_path_replay.rs`.
///
/// Recording a new cassette is `scripts/record-llm-cassette.sh`, run by hand.
/// There is deliberately no record mode wired into the harness: it would run
/// twice a year, and would itself need to be money-safe.
pub fn with_cassette<T>(
    home: &Path,
    cassette: &str,
    f: impl FnOnce(Command) -> T,
) -> (T, Vec<serde_json::Value>) {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/llm")
        .join(format!("{cassette}.json"));
    let body: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&fixture)
            .unwrap_or_else(|e| panic!("cassette {} unreadable: {e}", fixture.display())),
    )
    .unwrap_or_else(|e| panic!("cassette {} is not valid JSON: {e}", fixture.display()));

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime for the cassette server");
    let server = rt.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        server
    });

    let mut cmd = quorum(home);
    cmd.env("QUORUM_BASE_URL", server.uri())
        .env("QUORUM_ALLOW_PRIVATE_BASE_URL", "1")
        .env("QUORUM_API_KEY", "sk-cassette-not-a-real-key");

    let out = f(cmd);

    let sent = rt.block_on(async {
        server
            .received_requests()
            .await
            .expect("wiremock failed to record received requests")
            .iter()
            .map(|r| {
                serde_json::from_slice(&r.body)
                    .unwrap_or_else(|e| panic!("request body the binary sent is not JSON: {e}"))
            })
            .collect::<Vec<serde_json::Value>>()
    });

    drop(server);
    (out, sent)
}

/// Spawn `quorum` pointed at a real endpoint, spending real money.
///
/// Panics unless `QUORUM_TEST_LIVE=1` is set. A present `QUORUM_API_KEY` is
/// deliberately *not* sufficient -- present-key-means-live is the #501 bug and
/// must not survive in any form.
pub fn quorum_live(home: &Path) -> Command {
    let opted_in = std::env::var(LIVE_OPT_IN).is_ok_and(|v| v == "1");
    assert!(
        opted_in,
        "This test makes a real, paid LLM call. Set {LIVE_OPT_IN}=1 to allow it.\n\
         A present QUORUM_API_KEY is not enough on purpose -- see issue #501."
    );

    let api_key = std::env::var("QUORUM_API_KEY")
        .unwrap_or_else(|_| panic!("{LIVE_OPT_IN}=1 was set but QUORUM_API_KEY is not available"));

    let mut cmd = quorum(home);
    cmd.env("QUORUM_API_KEY", api_key);
    if let Ok(base) = std::env::var("QUORUM_BASE_URL") {
        cmd.env("QUORUM_BASE_URL", base);
    } else {
        cmd.env_remove("QUORUM_BASE_URL"); // fall back to the production default
    }
    if let Ok(model) = std::env::var("QUORUM_MODEL") {
        cmd.env("QUORUM_MODEL", model);
    }
    cmd
}
