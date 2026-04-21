//! Markdown splitter: one [`Chunk`] per H2 section.
//!
//! Conventions (see crate docs for the full spec):
//! - Splits at H2 only. H3+ stays inside the parent H2 section.
//! - Code fences (and their contents) pass through verbatim.
//! - Chunk IDs are `{source}:{source_path}:{slug}`; duplicate slugs within
//!   the same document disambiguate with `-2`, `-3`, ...
//! - Line ranges are 1-indexed inclusive into the original markdown.
//! - H2 sections whose content is pure whitespace are skipped.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

/// Doc subtype tag, stored on [`Chunk::subtype`].
#[derive(Debug, Clone, Copy)]
pub enum DocSubtype {
    Readme,
    Adr,
    Changelog,
    /// Generic fallback.
    Doc,
}

impl DocSubtype {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Readme => "README",
            Self::Adr => "ADR",
            Self::Changelog => "CHANGELOG",
            Self::Doc => "DOC",
        }
    }
}

/// Split a markdown document into one [`Chunk`] per top-level (H2) section.
///
/// See the module docs for the splitting contract.
pub fn split_markdown(
    md: &str,
    source_path: &str,
    source: &str,
    subtype: DocSubtype,
    commit_sha: &str,
    indexed_at: DateTime<Utc>,
) -> Vec<Chunk> {
    let h2_starts = collect_h2_starts(md);
    if h2_starts.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::with_capacity(h2_starts.len());
    let mut slug_counts: HashMap<String, u32> = HashMap::new();

    for (i, h2) in h2_starts.iter().enumerate() {
        // Section byte range runs from the start of this H2 to the start of
        // the next H2 (or EOF).
        let section_start = h2.byte_start;
        let section_end = h2_starts.get(i + 1).map_or(md.len(), |n| n.byte_start);

        // Trim trailing whitespace/newlines from the slice but keep the
        // original absolute byte offsets so line-number math stays correct.
        let raw = &md[section_start..section_end];
        let trimmed_len = raw.trim_end().len();
        let content_slice = &raw[..trimmed_len];

        // Skip sections whose body (everything after the heading line) is
        // whitespace-only. We still have to include the heading itself in
        // `content`, but an empty body means nothing worth indexing.
        if section_body_is_blank(content_slice) {
            continue;
        }

        let start_line = line_of_offset(md, section_start);
        // end_line is the last line that has any content in content_slice.
        let end_abs = section_start + trimmed_len;
        // If content_slice is empty we already skipped above, so end_abs >
        // section_start here.
        let end_line = line_of_offset(md, end_abs.saturating_sub(1));

        // Disambiguate duplicate slugs.
        let base_slug = slugify(&h2.heading_text);
        let count = slug_counts.entry(base_slug.clone()).or_insert(0);
        *count += 1;
        let slug = if *count == 1 {
            base_slug
        } else {
            format!("{base_slug}-{}", *count)
        };

        let id = format!("{source}:{source_path}:{slug}");

        chunks.push(Chunk {
            id,
            source: source.to_string(),
            kind: ChunkKind::Doc,
            subtype: Some(subtype.as_str().to_string()),
            qualified_name: None,
            signature: None,
            content: content_slice.to_string(),
            metadata: ChunkMeta {
                source_path: source_path.to_string(),
                line_range: LineRange {
                    start: start_line,
                    end: end_line,
                },
                commit_sha: commit_sha.to_string(),
                indexed_at,
                source_version: None,
                language: Some("markdown".into()),
                is_exported: true,
                neighboring_symbols: Vec::new(),
            },
            provenance: Provenance {
                extractor: "markdown-splitter".into(),
                confidence: 1.0,
                source_uri: format!("{source}:{source_path}"),
            },
        });
    }

    chunks
}

/// Record of an H2 heading's position in the source document.
struct H2Start {
    byte_start: usize,
    heading_text: String,
}

/// Walk the markdown stream and collect every H2 heading's byte offset + text.
fn collect_h2_starts(md: &str) -> Vec<H2Start> {
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH)
        .into_offset_iter();

    let mut starts = Vec::new();
    let mut current: Option<(usize, String)> = None;

    for (event, range) in parser {
        match event {
            Event::Start(Tag::Heading { level: HeadingLevel::H2, .. }) => {
                current = Some((range.start, String::new()));
            }
            Event::End(end_tag) if current.is_some() => {
                use pulldown_cmark::TagEnd;
                if matches!(end_tag, TagEnd::Heading(HeadingLevel::H2))
                    && let Some((start, text)) = current.take()
                {
                    starts.push(H2Start {
                        byte_start: start,
                        heading_text: text,
                    });
                }
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, text)) = current.as_mut() {
                    text.push_str(&t);
                }
            }
            _ => {}
        }
    }

    starts
}

/// Everything after the first line of the section is the "body". Return true
/// if that body is empty or whitespace-only.
fn section_body_is_blank(section: &str) -> bool {
    match section.find('\n') {
        Some(nl) => section[nl + 1..].trim().is_empty(),
        None => true, // heading-only, no body at all
    }
}

/// Convert a byte offset into a 1-indexed line number.
fn line_of_offset(md: &str, offset: usize) -> u32 {
    // `offset` may equal md.len() for the EOF sentinel; clamp for safety.
    let clamped = offset.min(md.len());
    let count = md[..clamped].matches('\n').count() + 1;
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_dash = true; // suppress leading dashes
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    if out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("section");
    }
    out
}
