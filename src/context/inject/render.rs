//! Render an [`InjectionPlan`] as a markdown block for LLM review prompts.
//!
//! Pure function: no I/O. The block is wrapped in a `<retrieved_reference>`
//! XML tag with a non-authoritative framing line, then a `# Context` header,
//! one card per injected chunk, a `---` rule, and a footer summary.
//!
//! The XML wrapper follows GPT-5 prompting guidance for steering attention
//! toward and away from reference material; the framing line tells the model
//! to treat retrieved chunks as *related* code rather than authoritative
//! patterns so it doesn't drift toward mimicking conventions that may
//! themselves be under review.

const FRAMING_HEADER: &str = "The following code is retrieved from the codebase for context. \
It shows how related components are currently implemented, but its patterns are not guaranteed to be correct or authoritative. \
Evaluate the code under review on its own merits.";

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::context::inject::plan::InjectionPlan;
use crate::context::inject::stale::StalenessAnnotator;
use crate::context::retrieve::PrecedenceLog;
use crate::context::types::ChunkKind;
use crate::prompt_sanitize::{
    defang_sandbox_tags, pick_fence_for, sanitize_fence_lang, sanitize_inline_metadata,
};

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
    out.push_str("<retrieved_reference>\n");
    out.push_str(FRAMING_HEADER);
    out.push_str("\n\n");
    out.push_str("# Context\n\n");

    for (i, scored) in plan.injected.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_card(&mut out, scored, staleness);
    }

    render_footer(&mut out, plan, precedence);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</retrieved_reference>\n");
    out
}

fn render_card(
    out: &mut String,
    scored: &crate::context::retrieve::ScoredChunk,
    staleness: &dyn StalenessAnnotator,
) {
    let chunk = &scored.chunk;
    let raw_path = &chunk.metadata.source_path;
    let start = chunk.metadata.line_range.start();
    let end = chunk.metadata.line_range.end();
    let raw_lang = chunk.metadata.language.as_deref().unwrap_or("");

    // Heading + blockquote metadata is interpolated into single-line markdown
    // constructs; strip newlines / backticks / control chars and defang any
    // sandbox closing tags so adversarial chunks can't break out of the
    // wrapper or terminate the inline-code span around qname.
    let path = sanitize_inline_metadata(raw_path);
    let source = sanitize_inline_metadata(&chunk.source);
    // Heading shows the language as a short identifier; reuse the fence-info
    // sanitizer so an adversarial language can't smuggle prose into the
    // heading line either.
    let heading_lang = sanitize_fence_lang(raw_lang);

    match chunk.kind {
        ChunkKind::Doc => {
            let _ = writeln!(out, "### Doc: {path}:{start}-{end}");
        }
        ChunkKind::Symbol | ChunkKind::Schema => {
            let qname = sanitize_inline_metadata(
                chunk.qualified_name.as_deref().unwrap_or("<anonymous>"),
            );
            let label = if matches!(chunk.kind, ChunkKind::Schema) {
                "Schema"
            } else {
                "Symbol"
            };
            let _ = writeln!(
                out,
                "### {label}: `{qname}` ({heading_lang}, {path}:{start}-{end})"
            );
        }
    }

    let short_sha = short_sha(&chunk.metadata.commit_sha);
    let _ = writeln!(out, "> Source: {source}, commit {short_sha}");

    if let Some(msg) = staleness.annotate(chunk) {
        let _ = writeln!(out, "> WARNING: {}", sanitize_inline_metadata(&msg));
    }

    out.push('\n');

    match chunk.kind {
        ChunkKind::Symbol | ChunkKind::Schema => {
            let safe = defang_sandbox_tags(&chunk.content);
            let fence = pick_fence_for(&safe);
            let fence_lang = sanitize_fence_lang(raw_lang);
            let _ = writeln!(out, "{fence}{fence_lang}");
            out.push_str(&safe);
            if !safe.ends_with('\n') {
                out.push('\n');
            }
            let _ = writeln!(out, "{fence}");
        }
        ChunkKind::Doc => {
            let demoted = demote_h2(&chunk.content);
            let safe = defang_sandbox_tags(&demoted);
            out.push_str(&safe);
            if !safe.ends_with('\n') {
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
                sanitize_inline_metadata(&entry.winner_source),
                sanitize_inline_metadata(&entry.loser_source),
                sanitize_inline_metadata(&entry.reason),
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

/// Demote shallow ATX headings (`# `, `## `) so no doc line outranks our
/// `###` card headers or the top-level `# Context` title. `### ` and deeper
/// pass through untouched.
fn demote_h2(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    for (i, line) in body.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(rest) = line.strip_prefix("## ") {
            out.push_str("#### ");
            out.push_str(rest);
        } else if let Some(rest) = line.strip_prefix("# ") {
            out.push_str("#### ");
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
    }
    out
}
