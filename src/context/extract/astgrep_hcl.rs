//! Terraform/HCL symbol extractor via ast-grep (library, not subprocess).
//!
//! Emits one [`Chunk`] of kind `Symbol` per top-level HCL block of type
//! `variable`, `output`, `resource`, or `module`. Other top-level blocks
//! (e.g. `terraform`, `provider`, `locals`, `data`) are skipped, as are
//! nested blocks inside resources (e.g. `lifecycle`, `dynamic`).
//!
//! qualified_name:
//! - `variable "NAME"` -> `NAME`
//! - `output   "NAME"` -> `NAME`
//! - `module   "NAME"` -> `NAME`
//! - `resource "TYPE" "NAME"` -> `TYPE.NAME`
//!
//! signature is the block header line (no body), e.g. `variable "cidr_block"`
//! or `resource "aws_vpc" "this"`.
//!
//! content is the `description` attribute value if present (variable/output),
//! else a whitespace-collapsed, length-capped rendering of the block body.

use ast_grep_config::{from_yaml_string, GlobalRules, RuleConfig};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

const RULE_YAMLS: &[&str] = &[include_str!("../../../rules/hcl/extraction/block.yml")];

/// Maximum characters kept for a body-derived `content` fallback.
const BODY_CONTENT_MAX: usize = 500;

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals)
            .map_err(|e| anyhow::anyhow!("failed to parse bundled hcl extraction rule: {e}"))?;
        rules.extend(parsed);
    }
    Ok(rules)
}

/// Extract Terraform/HCL symbols (variable, output, resource, module) from a
/// source file.
pub fn extract_hcl(
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
    let root = SupportLang::Hcl.ast_grep(src);

    let mut raw: Vec<ExtractedSymbol> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let node = m.get_node().clone();

            if !is_top_level_block(&node) {
                continue;
            }

            let Some(info) = block_header(&node, src) else {
                continue;
            };

            if !matches!(
                info.block_type.as_str(),
                "variable" | "output" | "resource" | "module"
            ) {
                continue;
            }

            let qualified_name = match info.block_type.as_str() {
                "resource" => match (info.label1.as_deref(), info.label2.as_deref()) {
                    (Some(t), Some(n)) => format!("{t}.{n}"),
                    (Some(t), None) => t.to_string(),
                    _ => continue,
                },
                _ => match info.label1.as_deref() {
                    Some(n) => n.to_string(),
                    None => continue,
                },
            };

            let signature = build_signature(&info);

            let description = find_description(&node, src);
            let content = match description {
                Some(d) if !d.is_empty() => d,
                _ => body_fallback_content(&node, src),
            };

            let byte_range = node.range();
            let start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;

            raw.push(ExtractedSymbol {
                byte_start: byte_range.start,
                qualified_name,
                start_line,
                end_line,
                signature,
                content,
            });
        }
    }

    raw.sort_by_key(|s| s.byte_start);

    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    raw.retain(|s| seen.insert((s.qualified_name.clone(), s.byte_start)));

    let mut name_counts: std::collections::HashMap<String, u32> =
        std::collections::HashMap::new();
    for s in &raw {
        *name_counts.entry(s.qualified_name.clone()).or_insert(0) += 1;
    }

    let all_items: Vec<(String, usize)> = raw
        .iter()
        .map(|s| (s.qualified_name.clone(), s.byte_start))
        .collect();

    let chunks: Vec<Chunk> = raw
        .into_iter()
        .map(|s| {
            let self_key = (s.qualified_name.clone(), s.byte_start);
            let neighboring_symbols: Vec<String> = all_items
                .iter()
                .filter(|k| **k != self_key)
                .map(|(n, _)| n.clone())
                .collect();

            let id = if name_counts.get(&s.qualified_name).copied().unwrap_or(1) > 1 {
                format!("{source}:{source_path}:{}@{}", s.qualified_name, s.byte_start)
            } else {
                format!("{source}:{source_path}:{}", s.qualified_name)
            };

            Chunk {
                id,
                source: source.to_string(),
                kind: ChunkKind::Symbol,
                subtype: None,
                qualified_name: Some(s.qualified_name.clone()),
                signature: Some(s.signature),
                content: s.content,
                metadata: ChunkMeta {
                    source_path: source_path.to_string(),
                    line_range: LineRange {
                        start: s.start_line,
                        end: s.end_line,
                    },
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("terraform".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance {
                    extractor: "ast-grep-hcl".to_string(),
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
    qualified_name: String,
    start_line: u32,
    end_line: u32,
    signature: String,
    content: String,
}

struct BlockHeader {
    block_type: String,
    label1: Option<String>,
    label2: Option<String>,
}

/// True if `block` is a top-level HCL block (its parent `body` is a direct
/// child of `config_file`). Nested blocks inside resources, like `lifecycle`,
/// return `false`.
fn is_top_level_block<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind().as_ref() != "body" {
        return false;
    }
    match parent.parent() {
        Some(gp) => gp.kind().as_ref() == "config_file",
        None => false,
    }
}

/// Pull the block-type identifier and up to two `string_lit` labels from the
/// block node's direct children.
fn block_header<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
    _src: &str,
) -> Option<BlockHeader> {
    let mut block_type: Option<String> = None;
    let mut labels: Vec<String> = Vec::new();

    for child in node.children() {
        let k = child.kind();
        let kind = k.as_ref();
        match kind {
            "identifier" if block_type.is_none() => {
                block_type = Some(child.text().into_owned());
            }
            "string_lit" => {
                labels.push(unquote_string_lit(child.text().as_ref()));
            }
            "body" => break,
            _ => {}
        }
    }

    let block_type = block_type?;
    let mut it = labels.into_iter();
    let label1 = it.next();
    let label2 = it.next();
    Some(BlockHeader {
        block_type,
        label1,
        label2,
    })
}

/// Strip surrounding double-quotes from an HCL `string_lit` text. Defensive:
/// returns the input unchanged if it isn't quoted on both sides.
fn unquote_string_lit(text: &str) -> String {
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text[1..text.len() - 1].to_string()
    } else {
        text.to_string()
    }
}

fn build_signature(info: &BlockHeader) -> String {
    match (&info.label1, &info.label2) {
        (Some(a), Some(b)) => format!("{} \"{a}\" \"{b}\"", info.block_type),
        (Some(a), None) => format!("{} \"{a}\"", info.block_type),
        _ => info.block_type.clone(),
    }
}

/// Look inside the block's `body` for an `attribute` whose identifier is
/// `description`, and return the attribute value as a plain string if it is a
/// simple quoted literal. Heredocs and non-string values return `None` and the
/// caller falls back to body text.
fn find_description<D: ast_grep_core::Doc>(
    block: &ast_grep_core::Node<'_, D>,
    src: &str,
) -> Option<String> {
    let body = block
        .children()
        .find(|c| c.kind().as_ref() == "body")?;

    for attr in body.children() {
        if attr.kind().as_ref() != "attribute" {
            continue;
        }
        // Attribute structure: identifier = expression
        let ident = attr
            .children()
            .find(|c| c.kind().as_ref() == "identifier")?;
        if ident.text().as_ref() != "description" {
            continue;
        }
        let expr = attr.children().find(|c| c.kind().as_ref() == "expression")?;
        if let Some(s) = first_string_literal(&expr, src) {
            return Some(s);
        }
        return None;
    }
    None
}

/// Walk an expression looking for a simple string literal; return its unquoted
/// content. Skips heredocs (starting with `<<`).
fn first_string_literal<D: ast_grep_core::Doc>(
    expr: &ast_grep_core::Node<'_, D>,
    src: &str,
) -> Option<String> {
    let range = expr.range();
    let text = src.get(range)?.trim();
    if text.starts_with("<<") {
        return None;
    }
    // The common shape is expression > literal_value > string_lit. Look for a
    // descendant string_lit and unquote it.
    fn find_string_lit<'a, D: ast_grep_core::Doc>(
        n: &ast_grep_core::Node<'a, D>,
    ) -> Option<ast_grep_core::Node<'a, D>> {
        if n.kind().as_ref() == "string_lit" {
            return Some(n.clone());
        }
        for c in n.children() {
            if let Some(f) = find_string_lit(&c) {
                return Some(f);
            }
        }
        None
    }
    let lit = find_string_lit(expr)?;
    Some(unquote_string_lit(lit.text().as_ref()))
}

/// Fallback content for blocks without a usable `description`: the text inside
/// `{ ... }`, whitespace collapsed and capped.
fn body_fallback_content<D: ast_grep_core::Doc>(
    block: &ast_grep_core::Node<'_, D>,
    src: &str,
) -> String {
    let body = match block.children().find(|c| c.kind().as_ref() == "body") {
        Some(b) => b,
        None => return String::new(),
    };
    let range = body.range();
    let raw = src.get(range).unwrap_or("").trim();
    let mut out = String::with_capacity(raw.len().min(BODY_CONTENT_MAX));
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
        if out.len() >= BODY_CONTENT_MAX {
            break;
        }
    }
    out.trim().to_string()
}
