//! Render an [`InjectionPlan`] as a markdown block for LLM review prompts.
//!
//! Pure function: no I/O. Layout is a `# Context` header, one card per
//! injected chunk, a `---` rule, and a footer summary.

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::context::inject::plan::InjectionPlan;
use crate::context::inject::stale::StalenessAnnotator;
use crate::context::retrieve::PrecedenceLog;
use crate::context::types::ChunkKind;

/// Render an injection plan as a markdown block. Returns an empty string
/// when the plan has no injected chunks.
#[must_use]
pub fn render_context_block(
    plan: &InjectionPlan,
    staleness: &dyn StalenessAnnotator,
    precedence: &PrecedenceLog,
) -> String {
    if plan.injected.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    out.push_str("# Context\n\n");

    for (i, scored) in plan.injected.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_card(&mut out, scored, staleness);
    }

    render_footer(&mut out, plan, precedence);
    out
}

fn render_card(
    out: &mut String,
    scored: &crate::context::retrieve::ScoredChunk,
    staleness: &dyn StalenessAnnotator,
) {
    let chunk = &scored.chunk;
    let path = &chunk.metadata.source_path;
    let start = chunk.metadata.line_range.start();
    let end = chunk.metadata.line_range.end();
    let lang = chunk.metadata.language.as_deref().unwrap_or("");

    match chunk.kind {
        ChunkKind::Doc => {
            let _ = writeln!(out, "### Doc: {path}:{start}-{end}");
        }
        ChunkKind::Symbol | ChunkKind::Schema => {
            let qname = chunk.qualified_name.as_deref().unwrap_or("<anonymous>");
            let label = if matches!(chunk.kind, ChunkKind::Schema) {
                "Schema"
            } else {
                "Symbol"
            };
            let _ = writeln!(
                out,
                "### {label}: `{qname}` ({lang}, {path}:{start}-{end})"
            );
        }
    }

    let short_sha = short_sha(&chunk.metadata.commit_sha);
    let _ = writeln!(out, "> Source: {}, commit {short_sha}", chunk.source);

    if let Some(msg) = staleness.annotate(chunk) {
        let _ = writeln!(out, "> WARNING: {msg}");
    }

    out.push('\n');

    match chunk.kind {
        ChunkKind::Symbol | ChunkKind::Schema => {
            let fence = fence_for(&chunk.content);
            let _ = writeln!(out, "{fence}{lang}");
            out.push_str(&chunk.content);
            if !chunk.content.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{fence}");
        }
        ChunkKind::Doc => {
            let demoted = demote_h2(&chunk.content);
            out.push_str(&demoted);
            if !demoted.ends_with('\n') {
                out.push('\n');
            }
        }
    }
}

fn render_footer(out: &mut String, plan: &InjectionPlan, precedence: &PrecedenceLog) {
    out.push_str("\n---\n");

    let unique_sources: HashSet<&str> = plan
        .injected
        .iter()
        .map(|c| c.chunk.source.as_str())
        .collect();

    let _ = writeln!(
        out,
        "{} tokens across {} chunks from {} source(s).",
        plan.token_count,
        plan.injected.len(),
        unique_sources.len()
    );

    if plan.below_threshold_count > 0 {
        let _ = writeln!(
            out,
            "{} candidate(s) below threshold (effective prose: {:.2}, adaptive: {}).",
            plan.below_threshold_count,
            plan.effective_prose_threshold,
            plan.adaptive_threshold_applied
        );
    }

    if !precedence.is_empty() {
        for entry in precedence.entries() {
            let _ = writeln!(
                out,
                "precedence: {} wins over {} ({})",
                entry.winner_source, entry.loser_source, entry.reason
            );
        }
    }
}

fn short_sha(sha: &str) -> &str {
    // Walk by char boundary so the slice is always valid, even if a caller
    // ever hands us a non-ASCII string (SHAs are hex, but the function is
    // cheap to make robust).
    match sha.char_indices().nth(7) {
        Some((idx, _)) => &sha[..idx],
        None => sha,
    }
}

/// Pick a fence length longer than any run of backticks appearing in `body`.
/// Markdown requires the closing fence to be at least as long as the opening,
/// and treats shorter runs inside as literal content.
fn fence_for(body: &str) -> String {
    let mut max_run = 0usize;
    let mut cur = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            cur += 1;
            if cur > max_run {
                max_run = cur;
            }
        } else {
            cur = 0;
        }
    }
    let len = max_run.max(2) + 1;
    "`".repeat(len)
}

/// Demote `## ` headers to `#### ` on a line-by-line basis so doc h2s don't
/// collide with our `###` card headers. Does not touch `### ` or deeper.
fn demote_h2(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(rest) = line.strip_prefix("## ") {
            out.push_str("#### ");
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
    }
    out
}
