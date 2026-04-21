//! Python symbol extractor via ast-grep (library, not subprocess).
//!
//! Emits one [`Chunk`] of kind `Symbol` per module-top-level `def` or `class`
//! whose name is considered exported. Exportedness follows PEP 8 / common
//! convention:
//!
//! - If a module-level `__all__` assignment exists, it is authoritative: a
//!   symbol is exported iff its name appears in `__all__`.
//! - Otherwise, a symbol is exported iff its name does NOT start with `_`.
//!
//! Docstrings live INSIDE the body: the first statement of a function or class
//! body, if it is a bare string literal, is the docstring.
//!
//! Limitations (accepted for MVP):
//! - Methods on classes, nested functions, and nested classes are NOT extracted.
//! - Async functions (`async def`) are treated identically to `def`.
//! - Conditional module-level defs (inside `if TYPE_CHECKING:` etc.) are
//!   extracted if they sit at module top level in the parse tree.

use ast_grep_config::{from_yaml_string, GlobalRules, RuleConfig};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/python/extraction/functions.yml"),
    include_str!("../../../rules/python/extraction/classes.yml"),
];

const DUNDER_ALL_RULE_YAML: &str =
    include_str!("../../../rules/python/extraction/dunder-all.yml");

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals)
            .map_err(|e| anyhow::anyhow!("failed to parse bundled python extraction rule: {e}"))?;
        rules.extend(parsed);
    }
    Ok(rules)
}

fn load_dunder_all_rule() -> anyhow::Result<RuleConfig<SupportLang>> {
    let globals = GlobalRules::default();
    let parsed = from_yaml_string::<SupportLang>(DUNDER_ALL_RULE_YAML, &globals)
        .map_err(|e| anyhow::anyhow!("failed to parse bundled python dunder-all rule: {e}"))?;
    parsed
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("dunder-all rule yaml produced no rule"))
}

/// Extract exported Python symbols (top-level `def`, `class`) from a source
/// file.
pub fn extract_python(
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
    let root = SupportLang::Python.ast_grep(src);

    let dunder_all_rule = load_dunder_all_rule()?;
    let dunder_all = find_dunder_all(src, &root, &dunder_all_rule);

    let mut raw: Vec<ExtractedSymbol> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let node = m.get_node();

            if !is_module_top_level(node.clone()) {
                continue;
            }

            let Some(name_node) = m.get_env().get_match("NAME") else {
                continue;
            };
            let name = name_node.text().into_owned();

            if !is_exported(&name, dunder_all.as_ref()) {
                continue;
            }

            let byte_range = node.range();
            let item_text = &src[byte_range.clone()];
            let signature = item_signature(item_text);

            let sig_start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;

            let docstring = extract_docstring(src, &node);

            let content = match &docstring {
                Some(d) if !d.is_empty() => d.clone(),
                _ => signature.clone(),
            };
            let content_start_line = sig_start_line;

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

    let mut seen: HashSet<(String, usize)> = HashSet::new();
    raw.retain(|s| seen.insert((s.name.clone(), s.byte_start)));

    let mut name_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
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
                    line_range: LineRange {
                        start: s.content_start_line,
                        end: s.end_line,
                    },
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("python".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance {
                    extractor: "ast-grep-python".to_string(),
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
    content_start_line: u32,
    end_line: u32,
    signature: String,
    content: String,
}

/// True if the given `function_definition` / `class_definition` node is at
/// module top level — i.e. no enclosing `function_definition` or
/// `class_definition` ancestor.
fn is_module_top_level<D: ast_grep_core::Doc>(node: ast_grep_core::Node<'_, D>) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        match p.kind().as_ref() {
            "function_definition" | "class_definition" => return false,
            _ => {}
        }
        cur = p.parent();
    }
    true
}

/// Decide if `name` is part of the file's public API.
fn is_exported(name: &str, dunder_all: Option<&HashSet<String>>) -> bool {
    match dunder_all {
        Some(set) => set.contains(name),
        None => !name.starts_with('_'),
    }
}

/// Find a module-level `__all__ = [...]` or `__all__ = (...)` and return the
/// set of string names. Returns `None` when no `__all__` is defined.
///
/// Uses ast-grep to match `assignment` nodes whose `left` field is `__all__`,
/// filters to module top-level (ignores assignments inside function/class
/// bodies), and extracts string literal children from the RHS `list` or
/// `tuple` expression. If multiple top-level assignments exist, the last one
/// wins (Python semantics).
///
/// Correctly skips occurrences inside comments, docstrings, and nested scopes
/// because those are not `assignment` AST nodes at module top level.
fn find_dunder_all<D: ast_grep_core::Doc>(
    src: &str,
    root: &ast_grep_core::AstGrep<D>,
    rule: &RuleConfig<SupportLang>,
) -> Option<HashSet<String>> {
    let mut top_level_matches: Vec<(usize, ast_grep_core::Node<'_, D>)> = Vec::new();
    for m in root.root().find_all(&rule.matcher) {
        let node = m.get_node().clone();
        // Confirm the left field text is exactly `__all__` (guard against
        // `self.__all__` etc. that the `pattern: __all__` might still match
        // via the wider rule).
        let Some(left) = node.field("left") else {
            continue;
        };
        if left.text().as_ref() != "__all__" {
            continue;
        }
        if !is_module_top_level(node.clone()) {
            continue;
        }
        top_level_matches.push((node.range().start, node));
    }

    if top_level_matches.is_empty() {
        return None;
    }

    // Python semantics: last assignment wins.
    top_level_matches.sort_by_key(|(start, _)| *start);
    let (_, last) = top_level_matches.into_iter().next_back()?;

    let rhs = last.field("right")?;

    let mut set = HashSet::new();
    for child in rhs.children() {
        if child.kind().as_ref() != "string" {
            continue;
        }
        if let Some(lit) = string_literal_text(src, &child) {
            set.insert(lit);
        }
    }
    Some(set)
}

/// Extract the content of a Python `string` node. tree-sitter-python wraps
/// string content in `string_start`, `string_content`, `string_end` children;
/// concatenate all `string_content` children. Fall back to trimming the outer
/// quote bytes if no `string_content` child is present.
fn string_literal_text<D: ast_grep_core::Doc>(
    src: &str,
    string_node: &ast_grep_core::Node<'_, D>,
) -> Option<String> {
    let mut out = String::new();
    let mut found = false;
    for c in string_node.children() {
        if c.kind().as_ref() == "string_content" {
            found = true;
            let range = c.range();
            out.push_str(&src[range]);
        }
    }
    if found {
        return Some(out);
    }
    let range = string_node.range();
    let raw = &src[range];
    // Strip one leading and trailing quote character if present.
    let trimmed = raw
        .strip_prefix('"')
        .or_else(|| raw.strip_prefix('\''))
        .unwrap_or(raw);
    let trimmed = trimmed
        .strip_suffix('"')
        .or_else(|| trimmed.strip_suffix('\''))
        .unwrap_or(trimmed);
    Some(trimmed.to_string())
}

/// Extract the signature (header) of a Python `def` or `class` item. Takes
/// everything up to (but not including) the `:` that opens the body, and
/// collapses whitespace runs to a single space.
///
/// Correctly skips `:` that appear inside type annotations, parentheses, or
/// brackets (e.g. `def f(x: dict[str, int]) -> None:`).
fn item_signature(item_text: &str) -> String {
    let bytes = item_text.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str: Option<u8> = None;
    let mut cut = item_text.len();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_str {
            if b == b'\\' {
                i += 2;
                continue;
            }
            if b == q {
                in_str = None;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' | b'\'' => in_str = Some(b),
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b':' if depth == 0 => {
                cut = i;
                break;
            }
            _ => {}
        }
        i += 1;
    }
    let raw = &item_text[..cut];

    // Collapse whitespace runs to a single space, then tidy up whitespace
    // around opening/closing brackets and commas so that
    // `def foo(\n  x,\n) -> int` becomes `def foo(x) -> int` rather than
    // `def foo( x, ) -> int`.
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
    let collapsed = out.trim().to_string();

    // Remove space immediately after `(` / `[` / `{` and before `)` / `]` /
    // `}` / `,`. Also drop a trailing comma before `)` / `]` / `}` (harmless
    // in Python, visually noisy in a one-line signature).
    let bytes = collapsed.as_bytes();
    let mut tidied = String::with_capacity(collapsed.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        // Drop `,` (and any following space) if the next non-space byte is a
        // closing bracket.
        if b == b',' {
            let mut k = i + 1;
            while bytes.get(k) == Some(&b' ') {
                k += 1;
            }
            if matches!(bytes.get(k), Some(b')') | Some(b']') | Some(b'}')) {
                i = k;
                continue;
            }
        }
        tidied.push(b as char);
        if matches!(b, b'(' | b'[' | b'{')
            && bytes.get(i + 1) == Some(&b' ')
        {
            i += 2;
            continue;
        }
        if b == b' ' {
            if let Some(&next) = bytes.get(i + 1) {
                if matches!(next, b')' | b']' | b'}' | b',') {
                    tidied.pop();
                }
            }
        }
        i += 1;
    }
    tidied
}

/// Inspect the body of a function/class node and, if the first statement is a
/// bare string literal, return its dedented text. Otherwise return `None`.
fn extract_docstring<D: ast_grep_core::Doc>(
    src: &str,
    node: &ast_grep_core::Node<'_, D>,
) -> Option<String> {
    // Locate the body `block` child.
    let block = node
        .children()
        .find(|c| c.kind().as_ref() == "block")?;

    // First `expression_statement` child whose child is a `string`.
    let expr_stmt = block
        .children()
        .find(|c| c.kind().as_ref() == "expression_statement")?;
    let string_node = expr_stmt
        .children()
        .find(|c| c.kind().as_ref() == "string")?;

    let range = string_node.range();
    let raw = &src[range];
    Some(dedent_docstring(raw))
}

/// Strip surrounding quotes (single or triple) and dedent using the minimal
/// common leading whitespace of non-empty interior lines. Trims blank leading
/// and trailing lines.
fn dedent_docstring(raw: &str) -> String {
    // Skip any string prefix like b/r/u/f (combinations thereof).
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if matches!(
            c,
            b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F'
        ) {
            i += 1;
        } else {
            break;
        }
    }
    let after_prefix = &raw[i..];

    // Detect triple-quoted vs single-quoted.
    let (inner, _triple) = if after_prefix.starts_with("\"\"\"") || after_prefix.starts_with("'''")
    {
        let q = &after_prefix[..3];
        let stripped = after_prefix
            .strip_prefix(q)
            .unwrap_or(after_prefix)
            .strip_suffix(q)
            .unwrap_or("");
        (stripped, true)
    } else if after_prefix.starts_with('"') || after_prefix.starts_with('\'') {
        let q = &after_prefix[..1];
        let stripped = after_prefix
            .strip_prefix(q)
            .unwrap_or(after_prefix)
            .strip_suffix(q)
            .unwrap_or("");
        (stripped, false)
    } else {
        (after_prefix, false)
    };

    // Split into lines.
    let lines: Vec<&str> = inner.split('\n').collect();

    // Compute minimal leading whitespace over non-empty lines *excluding* the
    // first line, which in Python convention has no indent (it's on the same
    // line as the opening quotes).
    let mut min_indent: Option<usize> = None;
    for line in lines.iter().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let n = line.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        min_indent = Some(match min_indent {
            Some(m) => m.min(n),
            None => n,
        });
    }
    let indent = min_indent.unwrap_or(0);

    let mut out_lines: Vec<String> = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        if idx == 0 {
            out_lines.push(line.trim_end().to_string());
        } else if line.len() >= indent {
            out_lines.push(line[indent..].trim_end().to_string());
        } else {
            out_lines.push(line.trim_end().to_string());
        }
    }

    // Drop leading/trailing blank lines.
    while out_lines.first().map(|l| l.is_empty()).unwrap_or(false) {
        out_lines.remove(0);
    }
    while out_lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        out_lines.pop();
    }

    out_lines.join("\n")
}
