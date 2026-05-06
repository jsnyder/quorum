//! TypeScript symbol extractor via ast-grep (library, not subprocess).
//!
//! Emits one [`Chunk`] of kind `Symbol` per top-level exported `function`,
//! `class`, `interface`, or `type` alias. Neighboring sibling symbols in the
//! same file are recorded on each chunk to give retrieval downstream a small
//! amount of local context.
//!
//! Limitations (accepted for MVP):
//! - Re-exports (`export { X } from "./y"`) are NOT extracted — they carry no
//!   symbol body and would duplicate the target's chunk.
//! - Default exports (`export default ...`) are NOT extracted.
//! - `export const`/`export let`/`export var` bindings are NOT extracted.
//! - Non-exported items are NOT extracted.

use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

/// Bundled extraction rule YAML. Compiled in so the extractor works without
/// depending on the filesystem layout at runtime.
const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/typescript/extraction/exported-functions.yml"),
    include_str!("../../../rules/typescript/extraction/exported-classes.yml"),
    include_str!("../../../rules/typescript/extraction/exported-interfaces.yml"),
    include_str!("../../../rules/typescript/extraction/exported-type-aliases.yml"),
];

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals).map_err(|e| {
            anyhow::anyhow!("failed to parse bundled typescript extraction rule: {e}")
        })?;
        rules.extend(parsed);
    }
    Ok(rules)
}

/// Extract exported TypeScript symbols (function, class, interface, type) from
/// a source file.
///
/// `source_path` is the path relative to the source root (used in chunk id and
/// metadata). `source` is the registered source name.
pub fn extract_typescript(
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
    let root = SupportLang::TypeScript.ast_grep(src);

    let mut raw: Vec<ExtractedSymbol> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let inner = m.get_node();
            let Some(name_node) = m.get_env().get_match("NAME") else {
                continue;
            };
            let name = name_node.text().into_owned();

            // The rule matches the inner declaration (function/class/interface/type_alias).
            // Walk up to the enclosing `export_statement` so the chunk's signature and
            // byte range include the `export` keyword.
            let node = match inner.parent() {
                Some(p) if p.kind() == "export_statement" => p,
                _ => continue,
            };
            let byte_range = node.range();
            let item_text = &src[byte_range.clone()];
            let signature = item_signature(item_text, &inner.kind());

            let sig_start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;

            let (doc, doc_start_line) = collect_preceding_jsdoc(src, byte_range.start);

            let (content, content_start_line) = if doc.is_empty() {
                (signature.clone(), sig_start_line)
            } else {
                (doc, doc_start_line.unwrap_or(sig_start_line))
            };

            raw.push(ExtractedSymbol {
                byte_start: byte_range.start,
                name,
                content_start_line,
                end_line,
                signature,
                content,
            });
        }
    }

    raw.sort_by_key(|s| s.byte_start);

    // Dedupe by (name, byte_start) so distinct items that happen to share a name
    // (e.g. `foo` in two sibling namespaces) are both preserved, but the same
    // node matched by two rules collapses to one.
    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    raw.retain(|s| seen.insert((s.name.clone(), s.byte_start)));

    let mut name_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for s in &raw {
        *name_counts.entry(s.name.clone()).or_insert(0) += 1;
    }

    let all_items: Vec<(String, usize)> =
        raw.iter().map(|s| (s.name.clone(), s.byte_start)).collect();

    let chunks: Vec<Chunk> = raw
        .into_iter()
        .map(|s| {
            let self_key = (s.name.clone(), s.byte_start);
            let neighboring_symbols: Vec<String> = all_items
                .iter()
                .filter(|k| **k != self_key)
                .map(|(n, _)| n.clone())
                .collect();

            let id = if name_counts.get(&s.name).copied().unwrap_or(1) > 1 {
                format!("{source}:{source_path}:{}@{}", s.name, s.byte_start)
            } else {
                format!("{source}:{source_path}:{}", s.name)
            };

            Chunk {
                id,
                source: source.to_string(),
                kind: ChunkKind::Symbol,
                subtype: None,
                qualified_name: Some(s.name.clone()),
                signature: Some(s.signature),
                content: s.content,
                metadata: ChunkMeta {
                    source_path: source_path.to_string(),
                    line_range: LineRange::new(s.content_start_line, s.end_line)
                        .expect("ast-grep-typescript extractor produced invalid line range"),
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("typescript".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance::new("ast-grep-typescript", 0.9, source_path.to_string())
                    .expect("ast-grep-typescript extractor produced invalid provenance"),
            }
        })
        .collect();

    Ok(chunks)
}

struct ExtractedSymbol {
    byte_start: usize,
    name: String,
    content_start_line: u32,
    end_line: u32,
    signature: String,
    content: String,
}

/// Extract the declaration signature from a full exported item's source text.
///
/// Strategy depends on the declaration kind so that a `{` on the RHS of a type
/// alias (`type P = { x: number }`) is not mistaken for a body opener:
/// - function/class/interface: truncate at the first `{` (the body).
/// - type_alias_declaration: truncate at the first `;` only; `{` is legal on
///   the RHS (object type) and must be preserved.
/// - any other kind: fall back to the first `{` or `;`.
/// Runs of whitespace collapse to a single space.
fn item_signature(item_text: &str, kind: &str) -> String {
    let end = match kind {
        "function_declaration" | "class_declaration" | "interface_declaration" => {
            item_text.find('{').unwrap_or(item_text.len())
        }
        "type_alias_declaration" => find_type_alias_semicolon(item_text),
        _ => item_text.find(['{', ';']).unwrap_or(item_text.len()),
    };
    let raw = &item_text[..end];
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

/// Scan a type-alias body for the terminating `;` while respecting string
/// and template literals as well as bracket depth. String/template state
/// prevents `;` inside literals (e.g. `type D = "a;b"`) from truncating the
/// signature. `<` / `>` count toward depth to keep generic constraints
/// grouped; this over-counts when `>` is a comparison operator, but that is
/// accepted as fuzzy for signature scanning.
fn find_type_alias_semicolon(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut str_quote: Option<u8> = None;
    let mut depth: i32 = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = str_quote {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                str_quote = None;
            }
        } else {
            match b {
                b'"' | b'\'' | b'`' => str_quote = Some(b),
                b'{' | b'(' | b'[' | b'<' => depth += 1,
                b'}' | b')' | b']' | b'>' => depth -= 1,
                b';' if depth <= 0 => return i,
                _ => {}
            }
        }
        i += 1;
    }
    text.len()
}

/// Walk backwards from `byte_start` collecting a single JSDoc block (`/** ... */`)
/// that immediately precedes the symbol. Returns the stripped comment text and
/// the 1-indexed line number where `/**` opens (`None` if no JSDoc was found).
///
/// Contiguity rule: only whitespace is permitted between the closing `*/` and
/// the symbol, and that whitespace must contain at most one newline (a blank
/// line — two or more consecutive newlines — breaks the association).
fn collect_preceding_jsdoc(src: &str, byte_start: usize) -> (String, Option<u32>) {
    // Scan backwards from byte_start over whitespace.
    let bytes = src.as_bytes();
    let mut i = byte_start;
    let mut newline_count: u32 = 0;
    while i > 0 {
        let b = bytes[i - 1];
        if b == b'\n' {
            newline_count += 1;
            if newline_count > 1 {
                // Blank line between comment and symbol — not contiguous.
                return (String::new(), None);
            }
            i -= 1;
        } else if b == b' ' || b == b'\t' || b == b'\r' {
            i -= 1;
        } else {
            break;
        }
    }

    // We expect the byte just before `i` to close a JSDoc block: `*/`.
    if i < 2 || &bytes[i - 2..i] != b"*/" {
        return (String::new(), None);
    }

    let close_start = i - 2;

    // Walk backwards to find the matching `/**`. We do a simple reverse search
    // from close_start; ast-grep has already parsed the file, so nested block
    // comments aren't a concern in standard TypeScript (they aren't legal anyway).
    let prefix = &src[..close_start];
    let open_idx = match prefix.rfind("/**") {
        Some(idx) => idx,
        None => return (String::new(), None),
    };

    // Guard: `/**` must appear at a logical comment start. If the byte directly
    // before `/**` is a letter, digit, or punctuation like `/` or `=`, the match
    // is almost certainly embedded in non-comment context (for example, a
    // regex literal `/a*/` on the preceding line that made `*/` look like a
    // JSDoc closer). In that case reject rather than attach unrelated text.
    if open_idx > 0 {
        let prev = bytes[open_idx - 1];
        if !matches!(prev, b'\n' | b'\r' | b' ' | b'\t') {
            return (String::new(), None);
        }
    }

    let inner = &src[open_idx + 3..close_start];
    let text = strip_jsdoc_inner(inner);
    let start_line = byte_to_line_1indexed(src, open_idx);
    (text, Some(start_line))
}

/// Strip the per-line `*` prefix from a JSDoc block's inner text (the substring
/// between `/**` and `*/`). Trims each interior line of leading whitespace and
/// an optional ` * ` or `*` marker, and drops empty leading/trailing lines.
fn strip_jsdoc_inner(inner: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in inner.lines() {
        let trimmed = line.trim_start();
        // Strip leading `*` and one optional space.
        let stripped = if let Some(rest) = trimmed.strip_prefix('*') {
            rest.strip_prefix(' ').unwrap_or(rest)
        } else {
            trimmed
        };
        lines.push(stripped.trim_end().to_string());
    }
    // Drop leading empty lines.
    while lines.first().map(|l| l.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    // Drop trailing empty lines.
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.join("\n")
}

fn byte_to_line_1indexed(src: &str, byte_offset: usize) -> u32 {
    let clamped = byte_offset.min(src.len());
    let newlines = src[..clamped].bytes().filter(|b| *b == b'\n').count();
    (newlines as u32) + 1
}
