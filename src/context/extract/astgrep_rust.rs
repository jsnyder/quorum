//! Rust symbol extractor via ast-grep (library, not subprocess).
//!
//! Emits one [`Chunk`] of kind `Symbol` per top-level `pub fn`, `pub struct`,
//! `pub enum`, or `pub trait`. Neighboring sibling symbols in the same file are
//! recorded on each chunk to give retrieval downstream a small amount of local
//! context.
//!
//! Limitations (accepted for MVP):
//! - Module visibility is not resolved. A `pub fn` inside a private `mod { ... }`
//!   is still extracted because ast-grep only sees the syntactic `pub`.
//! - Methods inside `impl` blocks, `use` statements, macros, consts, and type
//!   aliases are not extracted.

use ast_grep_config::{from_yaml_string, GlobalRules, RuleConfig};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

/// Bundled extraction rule YAML. Compiled in so the extractor works without
/// depending on the filesystem layout at runtime.
const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/rust/extraction/public-functions.yml"),
    include_str!("../../../rules/rust/extraction/public-structs.yml"),
    include_str!("../../../rules/rust/extraction/public-enums.yml"),
    include_str!("../../../rules/rust/extraction/public-traits.yml"),
];

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals)
            .map_err(|e| anyhow::anyhow!("failed to parse bundled rust extraction rule: {e}"))?;
        rules.extend(parsed);
    }
    Ok(rules)
}

/// Extract public Rust symbols (fn, struct, enum, trait) from a source file.
///
/// `source_path` is the path relative to the source root (used in chunk id
/// and metadata). `source` is the registered source name.
pub fn extract_rust(
    src: &str,
    source_path: &str,
    source: &str,
    commit_sha: &str,
    indexed_at: DateTime<Utc>,
) -> anyhow::Result<Vec<Chunk>> {
    if src.is_empty() {
        return Ok(Vec::new());
    }

    let rules = load_extraction_rules()?;
    let root = SupportLang::Rust.ast_grep(src);

    // (byte_start, name, node_start_line, node_end_line, signature, content)
    let mut raw: Vec<ExtractedSymbol> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let node = m.get_node();
            let Some(name_node) = m.get_env().get_match("NAME") else {
                continue;
            };
            let name = name_node.text().into_owned();

            // Byte range of the full item (e.g. entire function_item, struct_item, ...).
            let byte_range = node.range();
            let item_text = &src[byte_range.clone()];
            let signature = item_signature(item_text);

            // Start line where the signature begins (1-indexed).
            let sig_start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;

            // Collect preceding `///` doc comments (contiguous lines immediately above).
            let doc = collect_preceding_doc_comments(src, byte_range.start);

            let content = if doc.is_empty() {
                signature.clone()
            } else {
                doc
            };

            raw.push(ExtractedSymbol {
                byte_start: byte_range.start,
                name,
                sig_start_line,
                end_line,
                signature,
                content,
            });
        }
    }

    // Stable order by byte offset.
    raw.sort_by_key(|s| s.byte_start);

    // Defensive: dedupe by qualified_name, keeping the first occurrence.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    raw.retain(|s| seen.insert(s.name.clone()));

    // Compute neighboring symbols (names of OTHER symbols in this file, in order).
    let all_names: Vec<String> = raw.iter().map(|s| s.name.clone()).collect();

    let chunks: Vec<Chunk> = raw
        .into_iter()
        .map(|s| {
            let neighboring_symbols: Vec<String> = all_names
                .iter()
                .filter(|n| **n != s.name)
                .cloned()
                .collect();

            Chunk {
                id: format!("{source}:{source_path}:{}", s.name),
                source: source.to_string(),
                kind: ChunkKind::Symbol,
                subtype: None,
                qualified_name: Some(s.name.clone()),
                signature: Some(s.signature),
                content: s.content,
                metadata: ChunkMeta {
                    source_path: source_path.to_string(),
                    line_range: LineRange {
                        start: s.sig_start_line,
                        end: s.end_line,
                    },
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("rust".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance {
                    extractor: "ast-grep-rust".to_string(),
                    confidence: 0.9,
                    source_uri: source_path.to_string(),
                },
            }
        })
        .collect();

    Ok(chunks)
}

struct ExtractedSymbol {
    byte_start: usize,
    name: String,
    sig_start_line: u32,
    end_line: u32,
    signature: String,
    content: String,
}

/// Extract the declaration signature from a full item's source text.
///
/// Strategy: take everything up to the first `{` (for fn/struct/enum/trait bodies)
/// or `;` (for unit/tuple struct declarations), whichever comes first, and trim.
/// Multi-line signatures collapse runs of whitespace to a single space.
fn item_signature(item_text: &str) -> String {
    let end = item_text.find(['{', ';']).unwrap_or(item_text.len());
    let raw = &item_text[..end];
    // Normalize whitespace: collapse runs of whitespace to one space.
    let mut out = String::with_capacity(raw.len());
    let mut prev_ws = false;
    for c in raw.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// Walk backwards from `byte_start` collecting contiguous `///` doc-comment
/// lines that immediately precede the symbol. Returns the joined comment text
/// with leading `/// ` / `//!` markers stripped.
///
/// Contiguity rule: lines must be adjacent (no blank line gap). Leading
/// whitespace on each line is skipped.
fn collect_preceding_doc_comments(src: &str, byte_start: usize) -> String {
    // Find the start-of-line offset for byte_start.
    let prefix = &src[..byte_start];
    let mut cursor = prefix.rfind('\n').map(|n| n + 1).unwrap_or(0);

    // Walk upward one line at a time.
    let mut lines: Vec<&str> = Vec::new();
    while cursor > 0 {
        // Previous line spans [prev_line_start, cursor - 1) (excluding the \n at cursor-1).
        let slice = &src[..cursor - 1]; // drop the trailing newline
        let prev_line_start = slice.rfind('\n').map(|n| n + 1).unwrap_or(0);
        let line = &src[prev_line_start..cursor - 1];
        let trimmed = line.trim_start();
        if trimmed.starts_with("///") {
            lines.push(strip_doc_prefix(trimmed));
            cursor = prev_line_start;
        } else if trimmed.starts_with("//!") {
            // Inner doc comments on module-level items: treat the same way.
            lines.push(strip_doc_prefix(trimmed));
            cursor = prev_line_start;
        } else {
            break;
        }
    }

    lines.reverse();
    lines.join("\n")
}

fn strip_doc_prefix(line: &str) -> &str {
    // Strip `///` or `//!` plus one optional space.
    let after = if let Some(s) = line.strip_prefix("///") {
        s
    } else if let Some(s) = line.strip_prefix("//!") {
        s
    } else {
        line
    };
    after.strip_prefix(' ').unwrap_or(after)
}
