//! Issue #574: untrusted model output reaches a log only through `for_log`.
//!
//! Redaction is a chokepoint on the **outbound** path -- `post_json` redacts
//! every request body, so no LLM call can carry a secret out (#530). Logs are a
//! different sink and nothing covered them. The judge logged a 200-char prefix
//! of the raw response on two parse-failure paths, and #546 established that a
//! model can be talked into echoing text straight out of the file it was shown
//! -- so "the response" and "the reviewed source" are not separable categories.
//!
//! `redact::for_log` is the one way to put that text in a log: it redacts
//! first, then truncates (the other order can cut a secret in half and emit the
//! surviving half), and neutralises control characters so an attacker-shaped
//! response cannot rewrite a terminal.
//!
//! This guard exists because the fix is one call at each site, and one call is
//! exactly the kind of thing a new site forgets. #530 and #534 were both
//! properties that held on some paths because each implemented them separately.
//!
//! Scope: it scans source text under `src/`, so it catches the ordinary mistake
//! rather than a determined evasion -- the same ceiling as
//! `tests/no_env_mutation.rs` and `tests/spawn_helper_guard.rs`, and acceptable
//! for the same reason.

use std::path::Path;

/// Identifiers that hold raw model output or reviewed source at a log site.
///
/// Deliberately narrow: matching every variable called `text` would make the
/// guard noisy enough to be disabled, which is worse than a guard with a small
/// blind spot. These are the names the codebase actually uses for untrusted
/// payloads.
const UNTRUSTED: &[&str] = &[
    "response",
    "raw_response",
    "json_str",
    "raw_severity",
    "content",
    "completion",
];

fn rust_sources(dir: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            out.push((text, path));
        }
    }
}

/// Every `tracing::*!` invocation in `text`, as (line, body).
fn tracing_calls(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut consumed = 0usize;
    while let Some(at) = rest.find("tracing::") {
        let after = &rest[at..];
        let is_macro = ["warn!", "error!", "info!", "debug!", "trace!"]
            .iter()
            .any(|m| after[9..].starts_with(m));
        if is_macro {
            // Body runs to the matching close paren; a depth scan is enough
            // because these bodies do not contain unbalanced parens in strings
            // we care about, and over-capturing only makes the guard stricter.
            let mut depth = 0usize;
            let mut end = after.len();
            for (i, c) in after.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let line = text[..consumed + at].matches('\n').count() + 1;
            out.push((line, after[..end].to_string()));
        }
        consumed += at + 9;
        rest = &rest[at + 9..];
    }
    out
}

/// Blank out every `for_log( ... )` span so what remains is the text that
/// reaches the log record unwrapped. Balanced-paren scan; an unbalanced one
/// just blanks to the end, which errs toward silence rather than noise -- and
/// an unbalanced paren would not compile anyway.
fn strip_for_log_spans(body: &str) -> String {
    let mut out = body.to_string();
    while let Some(at) = out.find("for_log(") {
        let open = at + "for_log(".len() - 1;
        let mut depth = 0usize;
        let mut end = out.len();
        for (i, c) in out[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.replace_range(at..end.min(out.len()), " ");
    }
    out
}

#[test]
fn untrusted_model_output_reaches_logs_only_through_for_log() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(!files.is_empty(), "found no sources to scan");

    let mut offenders = Vec::new();
    for (text, path) in &files {
        for (line, body) in tracing_calls(text) {
            // Everything a `for_log(...)` call encloses is already handled,
            // so remove those spans and look at what is left. Quorum's review
            // of the first version caught two holes this closes at once:
            // checking `body.contains("for_log")` let one wrapped field exempt
            // an unwrapped one in the same call, and matching only `%name` /
            // `?name` / `= name` missed `warn!("raw: {}", response)` and
            // `warn!("raw: {response}")`, which carry the value just as well.
            let outside = strip_for_log_spans(&body);

            // The name has to be used as a value, not merely mentioned: the
            // message string "no JSON array found in LLM response" contains
            // `response` and is not a leak.
            let interpolates = |name: &str| {
                [
                    format!("%{name}"),
                    format!("?{name}"),
                    format!("= {name}"),
                    format!("{{{name}}}"),
                    format!(", {name}"),
                ]
                .iter()
                .any(|pat| {
                    let mut from = 0usize;
                    while let Some(at) = outside[from..].find(pat.as_str()) {
                        let end = from + at + pat.len();
                        let rest = &outside[end..];
                        let next = rest.chars().next();
                        // `response_len` is a different identifier, not a hit.
                        let is_longer_name = next.is_some_and(|c| c.is_alphanumeric() || c == '_');
                        // `response.len()` is a length, not the content. It is
                        // the only projection allowed through: anything else
                        // that reaches into the response is reaching for its
                        // text, and text goes through `for_log`.
                        let is_length = rest.starts_with(".len()");
                        if !is_longer_name && !is_length {
                            return true;
                        }
                        from = end;
                    }
                    false
                })
            };
            for name in UNTRUSTED {
                // No `body.contains("for_log")` clause: `interpolates` already
                // works on the text OUTSIDE those spans, and the old clause let
                // a single wrapped field exempt every unwrapped one beside it.
                if interpolates(name) {
                    let rel = path.strip_prefix(&src).unwrap_or(path);
                    offenders.push(format!("  {}:{line} interpolates `{name}`", rel.display()));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Untrusted model output must reach a log through `redact::for_log` (#574).\n\n\
         A model's response can echo text straight out of the reviewed file -- #546 \
         demonstrated it -- so a raw prefix in a log can carry a credential out of \
         the source under review. `for_log` redacts before truncating and strips \
         control characters.\n\n\
         Offending sites:\n{}",
        offenders.join("\n")
    );
}
