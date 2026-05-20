use ast_grep_config::{GlobalRules, RuleConfig, from_yaml_string};
use ast_grep_language::{LanguageExt, SupportLang};
use chrono::{DateTime, Utc};

use super::super::types::{Chunk, ChunkKind, ChunkMeta, LineRange, Provenance};

const RULE_YAMLS: &[&str] = &[
    include_str!("../../../rules/go/extraction/exported-functions.yml"),
    include_str!("../../../rules/go/extraction/exported-methods.yml"),
    include_str!("../../../rules/go/extraction/exported-structs.yml"),
    include_str!("../../../rules/go/extraction/exported-interfaces.yml"),
];

fn load_extraction_rules() -> anyhow::Result<Vec<RuleConfig<SupportLang>>> {
    let globals = GlobalRules::default();
    let mut rules = Vec::with_capacity(RULE_YAMLS.len());
    for yaml in RULE_YAMLS {
        let parsed = from_yaml_string::<SupportLang>(yaml, &globals)
            .map_err(|e| anyhow::anyhow!("failed to parse bundled go extraction rule: {e}"))?;
        rules.extend(parsed);
    }
    Ok(rules)
}

pub fn extract_go(
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
    let root = SupportLang::Go.ast_grep(src);

    let mut raw: Vec<(usize, String, u32, u32, String)> = Vec::new();

    for rule in &rules {
        for m in root.root().find_all(&rule.matcher) {
            let node = m.get_node();

            let base_name = if let Some(name_node) = m.get_env().get_match("NAME") {
                name_node.text().into_owned()
            } else {
                extract_go_name(node)
            };

            if base_name.is_empty() {
                continue;
            }

            let name = if node.kind().as_ref() == "method_declaration" {
                let receiver = extract_go_receiver_type(node);
                if receiver.is_empty() {
                    base_name
                } else {
                    format!("{receiver}.{base_name}")
                }
            } else {
                base_name
            };

            let byte_start = node.range().start;
            let start_line = (node.start_pos().line() as u32) + 1;
            let end_line = (node.end_pos().line() as u32) + 1;
            let item_text = &src[node.range()];
            let signature = go_item_signature(item_text);

            raw.push((byte_start, name, start_line, end_line, signature));
        }
    }

    raw.sort_by_key(|s| s.0);

    let mut seen: std::collections::HashSet<(String, usize)> = std::collections::HashSet::new();
    raw.retain(|s| seen.insert((s.1.clone(), s.0)));

    let all_names: Vec<String> = raw.iter().map(|s| s.1.clone()).collect();

    let chunks: Vec<Chunk> = raw
        .into_iter()
        .map(|(_byte_start, name, start_line, end_line, signature)| {
            let neighboring_symbols: Vec<String> =
                all_names.iter().filter(|n| **n != name).cloned().collect();

            let id = format!("{source}:{source_path}:{name}");

            Chunk {
                id,
                source: source.to_string(),
                kind: ChunkKind::Symbol,
                subtype: None,
                qualified_name: Some(name.clone()),
                signature: Some(signature.clone()),
                content: signature,
                metadata: ChunkMeta {
                    source_path: source_path.to_string(),
                    line_range: LineRange::new(start_line, end_line)
                        .expect("astgrep-go extractor produced invalid line range"),
                    commit_sha: commit_sha.to_string(),
                    indexed_at,
                    source_version: None,
                    language: Some("go".to_string()),
                    is_exported: true,
                    neighboring_symbols,
                },
                provenance: Provenance::new("ast-grep-go", 0.9, source_path.to_string())
                    .expect("ast-grep-go extractor produced invalid provenance"),
            }
        })
        .collect();

    Ok(chunks)
}

fn extract_go_name<D: ast_grep_core::Doc>(node: &ast_grep_core::Node<'_, D>) -> String {
    let kind = node.kind();
    let kind_str = kind.as_ref();

    // For function/method declarations, the name is a direct child
    if kind_str == "function_declaration" || kind_str == "method_declaration" {
        return node
            .children()
            .find(|c| {
                let k = c.kind();
                k.as_ref() == "identifier" || k.as_ref() == "field_identifier"
            })
            .map(|c| c.text().into_owned())
            .unwrap_or_default();
    }

    // For type_declaration, the name is inside type_spec > type_identifier
    if kind_str == "type_declaration"
        && let Some(spec) = node.children().find(|c| c.kind().as_ref() == "type_spec")
    {
        return spec
            .children()
            .find(|c| c.kind().as_ref() == "type_identifier")
            .map(|c| c.text().into_owned())
            .unwrap_or_default();
    }

    String::new()
}

pub fn extract_go_receiver_type<D: ast_grep_core::Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> String {
    node.children()
        .find(|c| c.kind().as_ref() == "parameter_list")
        .and_then(|pl| {
            pl.children()
                .find(|c| c.kind().as_ref() == "parameter_declaration")
        })
        .and_then(|pd| {
            pd.children().find(|c| {
                let k = c.kind();
                k.as_ref() == "type_identifier"
                    || k.as_ref() == "pointer_type"
                    || k.as_ref() == "qualified_type"
            })
        })
        .map(|t| {
            let text = t.text().into_owned();
            text.trim_start_matches('*').to_string()
        })
        .unwrap_or_default()
}

fn go_item_signature(item_text: &str) -> String {
    let end = item_text.find('{').unwrap_or(item_text.len());
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
