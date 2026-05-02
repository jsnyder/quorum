use crate::parser::Language;

/// Context gathered from AST for lines within a changed region.
#[derive(Debug, Clone, Default)]
pub struct HydrationContext {
    /// Signatures of functions called from within the changed region.
    pub callee_signatures: Vec<String>,
    /// Definitions of custom types used in the changed region.
    pub type_definitions: Vec<String>,
    /// Functions that call any function whose signature changed.
    pub callers: Vec<String>,
    /// Import/use targets referenced in the changed region.
    pub import_targets: Vec<String>,
    /// Bare identifiers for callees + imports referenced in the
    /// changed region. Used by structural retrieval for exact
    /// match against `chunks.qualified_name`. Complement to
    /// `callee_signatures` / `import_targets` which carry the
    /// richer LLM-facing context.
    pub qualified_names: Vec<String>,
}

/// Hydrate context for a set of changed line ranges within a single file.
pub fn hydrate(
    tree: &tree_sitter::Tree,
    source: &str,
    lang: Language,
    changed_lines: &[(u32, u32)],
) -> HydrationContext {
    if changed_lines.is_empty() {
        return HydrationContext::default();
    }

    let root = tree.root_node();
    let mut ctx = HydrationContext::default();

    // Collect all function definitions and type definitions in the file
    let func_kinds = function_def_kinds(lang);
    let type_kinds = type_def_kinds(lang);
    let call_kinds = call_expr_kinds(lang);
    let import_kinds = import_kinds(lang);

    let mut all_funcs: Vec<(String, String, u32, u32)> = Vec::new(); // (name, signature_text, start_line, end_line)
    let mut all_types: Vec<(String, String)> = Vec::new(); // (name, full_text)
    let mut all_imports: Vec<(String, String)> = Vec::new(); // (imported_name, full_text)

    // Gather definitions
    collect_definitions(&root, source, &func_kinds, &type_kinds, &import_kinds,
        &mut all_funcs, &mut all_types, &mut all_imports);

    // For each changed region, find function calls and resolve callees
    let mut seen_callees = std::collections::HashSet::new();
    let mut seen_types = std::collections::HashSet::new();

    for &(start, end) in changed_lines {
        collect_calls_in_range(&root, source, lang, &call_kinds, start, end,
            &all_funcs, &mut ctx.callee_signatures, &mut ctx.qualified_names, &mut seen_callees);

        collect_type_refs_in_range(&root, source, lang, &type_kinds, start, end,
            &all_types, &mut ctx.type_definitions, &mut seen_types);

        // Walk identifier-like leaf nodes (NOT comments / string literals) in the
        // changed range so import filtering can avoid matching tokens that look
        // like identifiers but live inside a comment or string body.
        let mut idents_in_range = std::collections::HashSet::new();
        collect_identifiers_in_range(&root, source, start, end, &mut idents_in_range);

        collect_import_refs_in_range(&root, source, &import_kinds, start, end,
            &all_imports, &idents_in_range,
            &mut ctx.import_targets, &mut ctx.qualified_names);
    }

    // Caller blast radius: if changed lines contain a function definition,
    // find all callers of that function in the file
    for &(start, end) in changed_lines {
        for (name, _, fstart, fend) in &all_funcs {
            if *fstart >= start && *fstart <= end {
                // This function's signature is in the changed region
                find_callers_of(&root, source, lang, &call_kinds, name, &all_funcs, &mut ctx.callers);
            }
        }
    }

    ctx
}

fn function_def_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["function_item"],
        Language::Python => vec!["function_definition"],
        Language::TypeScript | Language::Tsx => vec!["function_declaration", "method_definition"],
        Language::Yaml => vec![],
        Language::Bash => vec![],
        Language::Dockerfile => vec![],
        Language::Terraform => vec![],
    }
}

fn type_def_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["struct_item", "enum_item", "type_item"],
        Language::Python => vec!["class_definition"],
        Language::TypeScript | Language::Tsx => vec!["interface_declaration", "type_alias_declaration", "class_declaration"],
        Language::Yaml => vec![],
        Language::Bash => vec![],
        Language::Dockerfile => vec![],
        Language::Terraform => vec![],
    }
}

fn call_expr_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["call_expression"],
        Language::Python => vec!["call"],
        Language::TypeScript | Language::Tsx => vec!["call_expression"],
        Language::Yaml => vec![],
        Language::Bash => vec![],
        Language::Dockerfile => vec![],
        Language::Terraform => vec![],
    }
}

fn import_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["use_declaration"],
        Language::Python => vec!["import_statement", "import_from_statement"],
        Language::TypeScript | Language::Tsx => vec!["import_statement"],
        Language::Yaml => vec![],
        Language::Bash => vec![],
        Language::Dockerfile => vec![],
        Language::Terraform => vec![],
    }
}

fn collect_definitions(
    node: &tree_sitter::Node,
    source: &str,
    func_kinds: &[&str],
    type_kinds: &[&str],
    import_kinds_list: &[&str],
    funcs: &mut Vec<(String, String, u32, u32)>,
    types: &mut Vec<(String, String)>,
    imports: &mut Vec<(String, String)>,
) {
    let kind = node.kind();
    let line1 = node.start_position().row as u32 + 1;
    let line2 = node.end_position().row as u32 + 1;

    if func_kinds.contains(&kind) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = source[name_node.byte_range()].to_string();
            // Extract first line as signature
            let text = &source[node.byte_range()];
            let sig = text.lines().next().unwrap_or(text).to_string();
            funcs.push((name, sig, line1, line2));
        }
    }

    if type_kinds.contains(&kind) {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = source[name_node.byte_range()].to_string();
            let text = source[node.byte_range()].to_string();
            types.push((name, text));
        }
    }

    if import_kinds_list.contains(&kind) {
        let text = source[node.byte_range()].to_string();
        // Extract imported names from the text
        let names = extract_imported_names(&text);
        for name in names {
            imports.push((name, text.clone()));
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_definitions(&child, source, func_kinds, type_kinds, import_kinds_list,
                funcs, types, imports);
        }
    }
}

fn extract_imported_names(import_text: &str) -> Vec<String> {
    let mut names = Vec::new();
    // Rust: use std::collections::HashMap; -> "HashMap"
    // Python: from os.path import join -> "join"
    // Python: import sys -> "sys"
    let text = import_text.trim().trim_end_matches(';');

    if text.starts_with("use ") {
        // Rust: handle grouped `use std::collections::{HashMap, BTreeSet}` first.
        if let (Some(open), Some(close)) = (text.find('{'), text.rfind('}')) {
            if open < close {
                let inner = &text[open + 1..close];
                for part in inner.split(',') {
                    let part = part.trim();
                    if part.is_empty() || part == "*" {
                        continue;
                    }
                    // Handle `Foo as Bar` aliases inside the group.
                    let name = if let Some(alias) = part.split(" as ").nth(1) {
                        alias.trim()
                    } else {
                        // Handle nested paths like `io::{self, Read}` — take last segment.
                        part.rsplit("::").next().unwrap_or(part).trim()
                    };
                    if !name.is_empty() && name != "*" && name != "self" {
                        names.push(name.to_string());
                    } else if name == "self" {
                        // `use foo::{self, ...}` brings `foo` into scope; surface the
                        // parent segment (between "use " and "::{").
                        let head = &text[..open];
                        if let Some(parent) = head.rsplit("::").next() {
                            let parent = parent.trim();
                            if !parent.is_empty() {
                                names.push(parent.to_string());
                            }
                        }
                    }
                }
                return names;
            }
        }
        // Rust: last segment after ::, handle `as` aliases
        if let Some(last) = text.rsplit("::").next() {
            let name = last.trim().trim_end_matches(';');
            // Handle `use foo::bar as baz` -> "baz"
            let name = if let Some(alias) = name.split(" as ").nth(1) {
                alias.trim()
            } else {
                name
            };
            if !name.is_empty() && name != "*" {
                names.push(name.to_string());
            }
        }
    } else if text.starts_with("from ") {
        // Python: from X import a, b, c  /  from X import (a, b as c)
        if let Some(after_import) = text.split("import").nth(1) {
            let cleaned = after_import.trim().trim_start_matches('(').trim_end_matches(')');
            for part in cleaned.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                let name = if let Some(after_as) = part.split(" as ").nth(1) {
                    after_as.trim()
                } else {
                    part
                };
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    } else if text.starts_with("import ") {
        // TypeScript imports always include ` from `; Python `import sys` does not.
        // This lets us route the parse without language plumbing.
        let is_ts = text.contains(" from ");
        if is_ts {
            // Strip the trailing ` from "module"` (or `' '`) clause so we only parse
            // the import-clause portion.
            let clause_end = text.rfind(" from ").unwrap_or(text.len());
            let clause = text["import ".len()..clause_end].trim();

            // Split on the first top-level `{` to separate the default/namespace
            // half from the named-import group. Default imports come BEFORE the
            // brace; named imports come INSIDE it.
            let (head, group) = match clause.find('{') {
                Some(open) => {
                    let close = clause.rfind('}').unwrap_or(clause.len());
                    let inner = if close > open {
                        Some(clause[open + 1..close].to_string())
                    } else {
                        None
                    };
                    let head = clause[..open].trim().trim_end_matches(',').trim().to_string();
                    (head, inner)
                }
                None => (clause.to_string(), None),
            };

            // Head may contain: "" | "foo" | "* as ns" | "foo, * as ns"
            for part in head.split(',') {
                let part = part.trim();
                if part.is_empty() {
                    continue;
                }
                if let Some(after_as) = part.split(" as ").nth(1) {
                    // namespace or aliased default — local binding is after `as`.
                    let name = after_as.trim();
                    if !name.is_empty() && name != "*" {
                        names.push(name.to_string());
                    }
                } else if part != "*" {
                    // default import: bare identifier.
                    names.push(part.to_string());
                }
            }

            if let Some(inner) = group {
                for part in inner.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    // For `default as foo` re-export form OR `bar as baz`, prefer
                    // the local binding (after `as`).
                    let name = if let Some(after_as) = part.split(" as ").nth(1) {
                        after_as.trim()
                    } else {
                        part
                    };
                    if !name.is_empty() && name != "*" {
                        names.push(name.to_string());
                    }
                }
            }
            return names;
        }
        // Python: import sys / import foo.bar / import foo.bar as baz
        let module = text.trim_start_matches("import ").trim();
        let name = if let Some(after_as) = module.split(" as ").nth(1) {
            after_as.trim()
        } else {
            module.split('.').last().unwrap_or(module).trim()
        };
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    names
}

fn collect_calls_in_range(
    node: &tree_sitter::Node,
    source: &str,
    _lang: Language,
    call_kinds: &[&str],
    start_line: u32,
    end_line: u32,
    all_funcs: &[(String, String, u32, u32)],
    out: &mut Vec<String>,
    qnames_out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
) {
    let call_start = node.start_position().row as u32 + 1;
    let call_end = node.end_position().row as u32 + 1;

    // Overlap, not start-only: a multiline call expression like
    //     helper(
    //         1,
    //         2,
    //     )
    // starts before the changed range but its inner lines may be in-range.
    if call_kinds.contains(&node.kind()) && call_start <= end_line && call_end >= start_line {
        // Extract the function name being called
        if let Some(func_node) = node.child_by_field_name("function") {
            let func_text = &source[func_node.byte_range()];
            // Get the simple name (last identifier)
            let call_name = func_text.rsplit('.').next()
                .unwrap_or(func_text)
                .rsplit("::")
                .next()
                .unwrap_or(func_text)
                .trim();

            // Find matching definition
            if !seen.contains(call_name) {
                for (name, sig, _, _) in all_funcs {
                    if name == call_name {
                        out.push(sig.clone());
                        qnames_out.push(call_name.to_string());
                        seen.insert(call_name.to_string());
                        break;
                    }
                }
            }
        }
    }

    // Always recurse, regardless of whether this node was itself in-range —
    // tree-sitter's recursive descent already covers nested calls f(g(h())),
    // and we want to keep finding calls inside outer scopes that happen to
    // open before the changed range.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_calls_in_range(&child, source, _lang, call_kinds, start_line, end_line,
                all_funcs, out, qnames_out, seen);
        }
    }
}

fn collect_type_refs_in_range(
    node: &tree_sitter::Node,
    source: &str,
    _lang: Language,
    _type_kinds: &[&str],
    start_line: u32,
    end_line: u32,
    all_types: &[(String, String)],
    out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
) {
    let node_line = node.start_position().row as u32 + 1;

    // Look for type identifiers in the changed range
    if node_line >= start_line && node_line <= end_line && node.child_count() == 0 {
        let text = &source[node.byte_range()];
        // Check if this identifier matches a known type definition
        if !seen.contains(text) {
            for (name, full_def) in all_types {
                if name == text {
                    out.push(full_def.clone());
                    seen.insert(text.to_string());
                    break;
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_type_refs_in_range(&child, source, _lang, _type_kinds, start_line, end_line,
                all_types, out, seen);
        }
    }
}

/// Walk identifier-like leaf nodes inside [start_line, end_line], excluding
/// comments and string-literal interiors. Used by `collect_import_refs_in_range`
/// to scope imports to identifiers actually referenced in the changed range
/// without false-matching tokens that appear in comments or string content.
fn collect_identifiers_in_range(
    node: &tree_sitter::Node,
    source: &str,
    start_line: u32,
    end_line: u32,
    out: &mut std::collections::HashSet<String>,
) {
    let kind = node.kind();
    // Skip subtrees that cannot legitimately reference an imported name.
    // tree-sitter kind names are language-stable for these.
    if kind.contains("comment")
        || kind == "string_literal"
        || kind == "string"
        || kind == "raw_string_literal"
        || kind == "template_string"
    {
        return;
    }

    let n_start = node.start_position().row as u32 + 1;
    let n_end = node.end_position().row as u32 + 1;
    if n_end < start_line || n_start > end_line {
        return;
    }

    if node.child_count() == 0 {
        // Leaf — capture identifier-shaped text only when this leaf overlaps the range.
        if n_start <= end_line && n_end >= start_line {
            let text = &source[node.byte_range()];
            if is_identifier_like(text) {
                out.insert(text.to_string());
            }
        }
        return;
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_identifiers_in_range(&child, source, start_line, end_line, out);
        }
    }
}

fn is_identifier_like(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[allow(clippy::too_many_arguments)]
fn collect_import_refs_in_range(
    _node: &tree_sitter::Node,
    _source: &str,
    _import_kinds: &[&str],
    _start_line: u32,
    _end_line: u32,
    all_imports: &[(String, String)],
    idents_in_range: &std::collections::HashSet<String>,
    out: &mut Vec<String>,
    qnames_out: &mut Vec<String>,
) {
    // Per-call scoping: only surface imports whose imported name actually
    // appears as an identifier in the changed range. Avoids the previous
    // behaviour of dumping every file-level import on every review, which
    // dragged unrelated modules into the LLM prompt and broke import-target
    // filtering downstream.
    //
    // We rely on a tree-sitter identifier walk (collect_identifiers_in_range)
    // rather than a textual substring search, so identifier-shaped tokens
    // inside comments or string literals do not count as references.
    let mut seen = std::collections::HashSet::new();
    for (name, text) in all_imports {
        if seen.contains(name) {
            continue;
        }
        if idents_in_range.contains(name) {
            out.push(format!("{}: {}", name, text.trim()));
            qnames_out.push(name.clone());
            seen.insert(name.clone());
        }
    }
}

/// Parse a unified diff to extract changed line ranges per file.
/// Returns Vec<(file_path, Vec<(start_line, end_line)>)>
pub fn parse_unified_diff(diff: &str) -> Vec<(String, Vec<(u32, u32)>)> {
    let mut results = Vec::new();
    let mut current_file: Option<String> = None;
    let mut current_ranges: Vec<(u32, u32)> = Vec::new();

    for line in diff.lines() {
        if line.starts_with("+++ b/") {
            // Save previous file
            if let Some(file) = current_file.take() {
                if !current_ranges.is_empty() {
                    results.push((file, std::mem::take(&mut current_ranges)));
                }
            }
            current_file = Some(line[6..].to_string());
        } else if line.starts_with("@@ ") {
            // Parse @@ -old,count +new,count @@ format
            if let Some(plus_part) = line.split('+').nth(1) {
                let nums: Vec<&str> = plus_part
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .collect();
                if let Some(start_str) = nums.first() {
                    if let Ok(s) = start_str.parse::<u32>() {
                        // Count is optional in unified diff format (defaults to 1).
                        // "@@ -10 +10 @@" means a single-line change at line 10.
                        let count = nums
                            .get(1)
                            .and_then(|c| c.parse::<u32>().ok())
                            .unwrap_or(1);
                        // Pure-deletion hunks (+N,0) have count==0 and contribute no
                        // changed lines on the new side — skip rather than emit a
                        // garbage (N, N-1) range from saturating_sub underflow.
                        if count > 0 {
                            current_ranges.push((s, s + count - 1));
                        }
                    }
                }
            }
        }
    }
    // Save last file
    if let Some(file) = current_file {
        if !current_ranges.is_empty() {
            results.push((file, current_ranges));
        }
    }

    results
}

fn find_callers_of(
    node: &tree_sitter::Node,
    source: &str,
    _lang: Language,
    call_kinds: &[&str],
    target_name: &str,
    all_funcs: &[(String, String, u32, u32)],
    callers: &mut Vec<String>,
) {
    if call_kinds.contains(&node.kind()) {
        if let Some(func_node) = node.child_by_field_name("function") {
            let func_text = &source[func_node.byte_range()];
            let call_name = func_text.rsplit('.').next()
                .unwrap_or(func_text)
                .rsplit("::")
                .next()
                .unwrap_or(func_text)
                .trim();

            if call_name == target_name {
                // Find which function this call is inside
                let call_line = node.start_position().row as u32 + 1;
                for (fname, _, fstart, fend) in all_funcs {
                    if fname != target_name && call_line >= *fstart && call_line <= *fend {
                        if !callers.contains(fname) {
                            callers.push(fname.clone());
                        }
                    }
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            find_callers_of(&child, source, _lang, call_kinds, target_name, all_funcs, callers);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse, Language};

    // -- Callee signatures --

    #[test]
    fn hydrate_callee_signature_rust() {
        let source = "\
fn validate(input: &str) -> bool {
    !input.is_empty()
}

fn process(data: &str) {
    if validate(data) {
        println!(\"ok\");
    }
}
";
        let tree = parse(source, Language::Rust).unwrap();
        // Changed lines: process() at lines 5-8
        let ctx = hydrate(&tree, source, Language::Rust, &[(5, 8)]);
        assert!(
            ctx.callee_signatures.iter().any(|s| s.contains("validate") && s.contains("&str")),
            "Should find callee signature for validate(). Got: {:?}",
            ctx.callee_signatures
        );
    }

    #[test]
    fn hydrate_callee_signature_python() {
        let source = "\
def validate(text):
    return len(text) > 0

def process(data):
    if validate(data):
        print('ok')
";
        let tree = parse(source, Language::Python).unwrap();
        let ctx = hydrate(&tree, source, Language::Python, &[(4, 6)]);
        assert!(
            ctx.callee_signatures.iter().any(|s| s.contains("validate")),
            "Should find callee signature for validate(). Got: {:?}",
            ctx.callee_signatures
        );
    }

    #[test]
    fn hydrate_callee_signature_typescript() {
        let source = "\
function validate(input: string): boolean {
    return input.length > 0;
}

function process(data: string) {
    if (validate(data)) {
        console.log('ok');
    }
}
";
        let tree = parse(source, Language::TypeScript).unwrap();
        let ctx = hydrate(&tree, source, Language::TypeScript, &[(5, 9)]);
        assert!(
            ctx.callee_signatures.iter().any(|s| s.contains("validate")),
            "Should find callee signature for validate(). Got: {:?}",
            ctx.callee_signatures
        );
    }

    // -- Type definitions --

    #[test]
    fn hydrate_type_definition_rust() {
        let source = "\
struct Request {
    auth: Option<String>,
    path: String,
}

fn handle(req: Request) {
    println!(\"{}\", req.path);
}
";
        let tree = parse(source, Language::Rust).unwrap();
        // Changed lines: handle() at lines 6-8
        let ctx = hydrate(&tree, source, Language::Rust, &[(6, 8)]);
        assert!(
            ctx.type_definitions.iter().any(|s| s.contains("Request")),
            "Should find type definition for Request. Got: {:?}",
            ctx.type_definitions
        );
    }

    #[test]
    fn hydrate_type_definition_typescript() {
        let source = "\
interface User {
    name: string;
    email: string;
}

function greet(user: User) {
    console.log(user.name);
}
";
        let tree = parse(source, Language::TypeScript).unwrap();
        let ctx = hydrate(&tree, source, Language::TypeScript, &[(6, 8)]);
        assert!(
            ctx.type_definitions.iter().any(|s| s.contains("User")),
            "Should find type definition for User. Got: {:?}",
            ctx.type_definitions
        );
    }

    // -- Caller blast radius --

    #[test]
    fn hydrate_caller_blast_radius_rust() {
        let source = "\
fn helper() -> i32 {
    42
}

fn caller_a() {
    let x = helper();
}

fn caller_b() {
    let y = helper();
}

fn no_call() {
    let z = 1;
}
";
        let tree = parse(source, Language::Rust).unwrap();
        // Changed lines: helper() signature at lines 1-3
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 3)]);
        assert!(
            ctx.callers.iter().any(|s| s.contains("caller_a")),
            "Should find caller_a. Got: {:?}",
            ctx.callers
        );
        assert!(
            ctx.callers.iter().any(|s| s.contains("caller_b")),
            "Should find caller_b. Got: {:?}",
            ctx.callers
        );
        assert!(
            !ctx.callers.iter().any(|s| s.contains("no_call")),
            "Should NOT find no_call"
        );
    }

    // -- Import targets --

    #[test]
    fn hydrate_import_targets_rust() {
        let source = "\
use std::collections::HashMap;
use std::io::Read;

fn process() {
    let mut map = HashMap::new();
}
";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(4, 6)]);
        assert!(
            ctx.import_targets.iter().any(|s| s.contains("HashMap")),
            "Should find HashMap import. Got: {:?}",
            ctx.import_targets
        );
    }

    #[test]
    fn hydrate_import_targets_python() {
        let source = "\
from os.path import join
import sys

def process():
    path = join('/tmp', 'test')
";
        let tree = parse(source, Language::Python).unwrap();
        let ctx = hydrate(&tree, source, Language::Python, &[(4, 5)]);
        assert!(
            ctx.import_targets.iter().any(|s| s.contains("join")),
            "Should find join import. Got: {:?}",
            ctx.import_targets
        );
    }

    // -- Edge cases --

    #[test]
    fn hydrate_no_context_for_unchanged_lines() {
        let source = "\
fn foo() { }
fn bar() { foo(); }
";
        let tree = parse(source, Language::Rust).unwrap();
        // Changed lines: only foo() at line 1
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 1)]);
        // foo doesn't call anything, so callee_signatures should be empty
        assert!(ctx.callee_signatures.is_empty());
    }

    #[test]
    fn hydrate_recursive_call_no_infinite_loop() {
        let source = "\
fn recurse(n: i32) -> i32 {
    if n <= 0 { return 0; }
    recurse(n - 1)
}
";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 4)]);
        // Should not infinite loop; callee is itself
        assert!(ctx.callee_signatures.len() <= 1);
    }

    #[test]
    fn hydrate_missing_definition_graceful() {
        let source = "\
fn process() {
    external_crate_fn();
}
";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 3)]);
        // external_crate_fn not defined in file — should not crash, just empty
        assert!(ctx.callee_signatures.is_empty() || ctx.callee_signatures.iter().all(|s| !s.contains("fn external_crate_fn")));
    }

    #[test]
    fn hydrate_empty_changed_lines() {
        let source = "fn foo() { }";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[]);
        assert!(ctx.callee_signatures.is_empty());
        assert!(ctx.type_definitions.is_empty());
        assert!(ctx.callers.is_empty());
        assert!(ctx.import_targets.is_empty());
    }

    // -- Diff-aware hydration tests --

    #[test]
    fn hydrate_diff_range_smaller_than_full_file() {
        let source = "\
fn unrelated() -> i32 { 42 }

fn validate(input: &str) -> bool { !input.is_empty() }

fn process(data: &str) {
    if validate(data) {
        println!(\"ok\");
    }
}

fn another_unrelated() -> i32 { 99 }
";
        let tree = parse(source, Language::Rust).unwrap();

        // Full file hydration
        let total_lines = source.lines().count() as u32;
        let full = hydrate(&tree, source, Language::Rust, &[(1, total_lines)]);

        // Diff-aware: only lines 5-9 changed (the process function)
        let diff = hydrate(&tree, source, Language::Rust, &[(5, 9)]);

        // Diff hydration should find callee (validate) but NOT unrelated functions
        assert!(diff.callee_signatures.iter().any(|s| s.contains("validate")));
        // Full hydration gets everything
        assert!(full.callee_signatures.len() >= diff.callee_signatures.len());
    }

    // -- Diff parser tests --

    #[test]
    fn parse_diff_extracts_ranges() {
        let diff = "\
--- a/src/auth.py
+++ b/src/auth.py
@@ -10,5 +10,8 @@ def login():
+    validate(user)
+    check_token(token)
+    return True
--- a/src/db.py
+++ b/src/db.py
@@ -20,3 +20,5 @@ def query():
+    cursor.execute(sql)
";
        let parsed = parse_unified_diff(diff);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "src/auth.py");
        assert_eq!(parsed[0].1[0], (10, 17)); // +10,8
        assert_eq!(parsed[1].0, "src/db.py");
        assert_eq!(parsed[1].1[0], (20, 24)); // +20,5
    }

    #[test]
    fn parse_diff_empty() {
        assert!(parse_unified_diff("").is_empty());
    }

    #[test]
    fn parse_diff_multiple_hunks_same_file() {
        let diff = "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -5,3 +5,4 @@ fn foo():
+    bar()
@@ -20,2 +21,5 @@ fn baz():
+    qux()
";
        let parsed = parse_unified_diff(diff);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "src/main.rs");
        assert_eq!(parsed[0].1.len(), 2);
        assert_eq!(parsed[0].1[0], (5, 8));   // +5,4
        assert_eq!(parsed[0].1[1], (21, 25));  // +21,5
    }

    #[test]
    fn hydrate_exposes_bare_callee_qualified_names() {
        // Structural retrieval needs bare identifiers to match
        // chunks.qualified_name. The full signature is useful for
        // the LLM prompt; the bare name is useful for lookup.
        let source = "\
fn validate(input: &str) -> bool {
    !input.is_empty()
}

fn process(data: &str) {
    if validate(data) {
        println!(\"ok\");
    }
}
";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(5, 8)]);
        assert!(
            ctx.qualified_names.iter().any(|n| n == "validate"),
            "expected bare 'validate' in qualified_names, got {:?}",
            ctx.qualified_names
        );
    }

    // -- #175: hydrate handles multi-byte UTF-8 source (paper-bug regression) --

    #[test]
    fn hydrate_correctly_processes_source_with_multibyte_utf8() {
        let source = "// Greeting: こんにちは 🦀\n\
                      fn helper() -> String { \"x\".to_string() }\n\
                      fn process(input: &str) -> String {\n\
                      \x20   let _ = helper();\n\
                      \x20   input.to_string()\n\
                      }\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();

        // Range [(4,4)] covers `let _ = helper();`. Line 1's multibyte chars
        // would expose any line-to-byte arithmetic that ignored UTF-8 boundaries.
        let ctx = hydrate(&tree, source, Language::Rust, &[(4, 4)]);

        // Positive assertion: the function ran to completion AND produced
        // expected results. Without this, a swallowed panic returning
        // Default would pass a no-panic check vacuously.
        assert!(
            ctx.callee_signatures.iter().any(|s| s.starts_with("fn helper")),
            "expected `helper` callee even when source contains multibyte UTF-8; got {:?}",
            ctx.callee_signatures
        );
    }

    #[test]
    fn hydrate_does_not_panic_when_change_range_contains_emoji() {
        let source = "fn greet() -> &'static str {\n\
                      \x20   \"こんにちは 🦀\"\n\
                      }\n\
                      fn caller() { greet(); }\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(2, 2)]); // emoji line
        // Hydration may or may not pick up greet's signature here — but it MUST NOT panic.
        let _ = ctx;
    }

    // -- #174: import_targets scoping to changed range --

    #[test]
    fn import_targets_only_includes_imports_referenced_in_changed_range() {
        let source = "use std::collections::HashMap;\n\
                      use std::sync::Arc;\n\
                      fn touched() {\n\
                      \x20   let _: Arc<u32> = Arc::new(0);\n\
                      }\n\
                      fn untouched() {\n\
                      \x20   let _: HashMap<String, u32> = HashMap::new();\n\
                      }\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        // Only line 4 (Arc usage in `touched`) changes.
        let ctx = hydrate(&tree, source, Language::Rust, &[(4, 4)]);
        let arc_count = ctx.import_targets.iter()
            .filter(|i| i.ends_with("::Arc") || i.as_str() == "Arc" || i.starts_with("Arc:"))
            .count();
        let hashmap_count = ctx.import_targets.iter()
            .filter(|i| i.ends_with("::HashMap") || i.as_str() == "HashMap" || i.starts_with("HashMap:"))
            .count();
        assert_eq!(arc_count, 1, "Arc must be hydrated exactly once; got {:?}", ctx.import_targets);
        assert_eq!(hashmap_count, 0, "HashMap must NOT be hydrated; got {:?}", ctx.import_targets);
        assert!(!ctx.import_targets.is_empty(), "import_targets unexpectedly empty");
    }

    #[test]
    fn import_targets_symmetric_when_changed_range_covers_other_function() {
        let source = "use std::collections::HashMap;\n\
                      use std::sync::Arc;\n\
                      fn touched() {\n\
                      \x20   let _: Arc<u32> = Arc::new(0);\n\
                      }\n\
                      fn untouched() {\n\
                      \x20   let _: HashMap<String, u32> = HashMap::new();\n\
                      }\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(7, 7)]); // HashMap line
        let arc_count = ctx.import_targets.iter()
            .filter(|i| i.ends_with("::Arc") || i.as_str() == "Arc" || i.starts_with("Arc:"))
            .count();
        let hashmap_count = ctx.import_targets.iter()
            .filter(|i| i.ends_with("::HashMap") || i.as_str() == "HashMap" || i.starts_with("HashMap:"))
            .count();
        assert_eq!(arc_count, 0, "Arc must NOT be hydrated when HashMap line is changed");
        assert_eq!(hashmap_count, 1, "HashMap must be hydrated; got {:?}", ctx.import_targets);
    }

    // -- #173: TypeScript default/namespace import bindings --

    #[test]
    fn extract_imported_names_typescript_default_import_uses_local_binding() {
        let names = extract_imported_names("import foo from \"x\";");
        assert_eq!(names, vec!["foo".to_string()], "got {names:?}");
    }

    #[test]
    fn extract_imported_names_typescript_mixed_default_and_named() {
        let names = extract_imported_names("import foo, { bar, baz } from \"x\";");
        assert_eq!(names, vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
            "mixed default+named must yield local binding plus named members; got {names:?}");
    }

    #[test]
    fn extract_imported_names_typescript_namespace_import() {
        let names = extract_imported_names("import * as ns from \"x\";");
        assert_eq!(names, vec!["ns".to_string()], "got {names:?}");
    }

    #[test]
    fn extract_imported_names_typescript_default_with_namespace() {
        let names = extract_imported_names("import foo, * as ns from \"x\";");
        assert_eq!(names, vec!["foo".to_string(), "ns".to_string()], "got {names:?}");
    }

    // -- #172: extract_imported_names splits Rust grouped use --

    #[test]
    fn extract_imported_names_splits_rust_grouped_use() {
        let names = extract_imported_names("use std::collections::{HashMap, BTreeSet};");
        assert_eq!(names, vec!["HashMap".to_string(), "BTreeSet".to_string()]);
    }

    // -- #170: collect_calls_in_range overlap (not start-only) --

    #[test]
    fn collect_calls_in_range_finds_call_when_only_inner_line_changed() {
        let source = "fn helper(a: i32, b: i32) -> i32 { a + b }\n\
                      fn caller() {\n\
                      \x20   helper(\n\
                      \x20       1,\n\
                      \x20       2,\n\
                      \x20   );\n\
                      }\n";
        // helper() spans lines 3..=6 (the call expression). Only line 4 (the "1," argument) is in the changed range.
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(4, 4)]);
        let helper_sigs: Vec<_> = ctx.callee_signatures.iter()
            .filter(|s| s.starts_with("fn helper"))
            .collect();
        assert_eq!(helper_sigs.len(), 1,
            "expected exactly one `fn helper` signature; got {:?}", ctx.callee_signatures);
    }

    #[test]
    fn collect_calls_in_range_negative_control_does_not_hydrate_callees_outside_range() {
        // Same source; range [(1,1)] covers only the `fn helper` definition line.
        // No CALL of helper exists in that range, so callee_signatures must be empty.
        let source = "fn helper(a: i32, b: i32) -> i32 { a + b }\n\
                      fn caller() {\n\
                      \x20   helper(\n\
                      \x20       1,\n\
                      \x20       2,\n\
                      \x20   );\n\
                      }\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_rust::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 1)]);
        assert!(ctx.callee_signatures.iter().all(|s| !s.starts_with("fn helper")),
            "range [(1,1)] should not hydrate `helper` as a callee; got {:?}", ctx.callee_signatures);
    }

    // -- #171: parse_unified_diff omitted-count handling --

    #[test]
    fn parse_unified_diff_handles_omitted_count_in_hunk_header() {
        // Single-line hunks omit the ",1" count: "@@ -10 +10 @@"
        let diff = "diff --git a/file.rs b/file.rs\n--- a/file.rs\n+++ b/file.rs\n@@ -10 +10 @@\n-old\n+new\n";
        let result = parse_unified_diff(diff);
        assert_eq!(result.len(), 1, "expected one file");
        assert_eq!(result[0].0, "file.rs");
        assert_eq!(result[0].1, vec![(10, 10)], "single-line hunk should yield (10, 10)");
    }

    #[test]
    fn parse_unified_diff_does_not_panic_on_signed_line_numbers() {
        // The "-" prefix in "-10" must not mis-parse as negative or panic.
        let diff = "+++ b/x.rs\n@@ -10 +10 @@\n";
        let _ = parse_unified_diff(diff); // must not panic
    }

    #[test]
    fn parse_unified_diff_handles_asymmetric_omitted_count() {
        // -1,3 has count, +5 omits count (single-line add).
        let diff = "+++ b/x.rs\n@@ -1,3 +5 @@\n-a\n-b\n-c\n+x\n";
        let result = parse_unified_diff(diff);
        assert_eq!(result, vec![("x.rs".into(), vec![(5, 5)])]);
    }

    #[test]
    fn parse_unified_diff_handles_pure_deletion_hunk() {
        // +N,0 = pure deletion at line N. Must not produce a (N, N-1) garbage range.
        let diff = "+++ b/y.rs\n@@ -10,3 +10,0 @@\n-a\n-b\n-c\n";
        let result = parse_unified_diff(diff);
        // Either the hunk is filtered out entirely, OR the range collapses to (N, N).
        // Author's choice; document and assert one.
        if let Some((_, ranges)) = result.first() {
            for &(s, e) in ranges {
                assert!(s <= e, "saturating_sub produced inverted range ({s}, {e})");
            }
        }
    }

    #[test]
    fn hydrate_exposes_bare_import_qualified_names() {
        let source = "\
use std::collections::HashMap;

fn uses_map() {
    let _m: HashMap<String, u32> = HashMap::new();
}
";
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(3, 5)]);
        assert!(
            ctx.qualified_names.iter().any(|n| n == "HashMap"),
            "expected bare 'HashMap' in qualified_names, got {:?}",
            ctx.qualified_names
        );
    }

    // -- Caller blast radius scope tests (#178) --

    #[test]
    fn caller_blast_radius_ignores_body_only_edits() {
        let source = "fn helper() -> i32 {\n    42\n}\n\nfn caller() {\n    helper();\n}\n";
        // Line 1 = `fn helper()...`, line 2 = `    42`, line 3 = `}`
        // Only line 2 (body) is changed -- should NOT trigger caller search
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(2, 2)]);
        assert!(ctx.callers.is_empty(), "body-only edit should not trigger caller blast radius");
    }

    #[test]
    fn caller_blast_radius_triggers_on_signature_edit() {
        let source = "fn helper() -> i32 {\n    42\n}\n\nfn caller() {\n    helper();\n}\n";
        // Line 1 = signature of `helper` -- SHOULD trigger caller search
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(1, 1)]);
        assert!(!ctx.callers.is_empty(), "signature edit should trigger caller blast radius");
    }

    #[test]
    fn caller_blast_radius_wide_body_edit_no_trigger() {
        let source = "fn big_fn(x: i32) -> i32 {\n    let a = x + 1;\n    let b = a * 2;\n    let c = b - 3;\n    c\n}\n\nfn user() {\n    big_fn(5);\n}\n";
        // Lines 2-5 are body only, signature is line 1
        let tree = parse(source, Language::Rust).unwrap();
        let ctx = hydrate(&tree, source, Language::Rust, &[(2, 5)]);
        assert!(ctx.callers.is_empty(), "wide body edit should not trigger caller blast radius");
    }

    // -- #179: Python import parsing returns correct local names --

    #[test]
    fn python_from_import_as_returns_local_binding() {
        let names = extract_imported_names("from os.path import join as pjoin, exists");
        assert_eq!(names, vec!["pjoin", "exists"]);
    }

    #[test]
    fn python_from_import_parenthesized() {
        let names = extract_imported_names("from os import (path, getcwd, listdir)");
        assert_eq!(names, vec!["path", "getcwd", "listdir"]);
    }

    #[test]
    fn python_from_import_parenthesized_with_alias_and_trailing_comma() {
        let names = extract_imported_names("from os import (path as p, getcwd,)");
        assert_eq!(names, vec!["p", "getcwd"]);
    }

    #[test]
    fn python_from_import_mixed_alias_and_plain() {
        let names = extract_imported_names("from x import foo, bar as b");
        assert_eq!(names, vec!["foo", "b"]);
    }

    #[test]
    fn python_import_as_returns_alias() {
        let names = extract_imported_names("import foo.bar as baz");
        assert_eq!(names, vec!["baz"]);
    }

    #[test]
    fn python_import_dotted_no_alias() {
        let names = extract_imported_names("import os.path");
        assert_eq!(names, vec!["path"]);
    }

    #[test]
    fn python_import_simple_no_alias() {
        let names = extract_imported_names("import sys");
        assert_eq!(names, vec!["sys"]);
    }
}
