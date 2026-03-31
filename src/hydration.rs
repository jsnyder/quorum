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
            &all_funcs, &mut ctx.callee_signatures, &mut seen_callees);

        collect_type_refs_in_range(&root, source, lang, &type_kinds, start, end,
            &all_types, &mut ctx.type_definitions, &mut seen_types);

        collect_import_refs_in_range(&root, source, &import_kinds, start, end,
            &all_imports, &mut ctx.import_targets);
    }

    // Caller blast radius: if changed lines contain a function definition,
    // find all callers of that function in the file
    for &(start, end) in changed_lines {
        for (name, _, fstart, fend) in &all_funcs {
            if *fstart <= end && *fend >= start {
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
    }
}

fn type_def_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["struct_item", "enum_item", "type_item"],
        Language::Python => vec!["class_definition"],
        Language::TypeScript | Language::Tsx => vec!["interface_declaration", "type_alias_declaration", "class_declaration"],
        Language::Yaml => vec![],
    }
}

fn call_expr_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["call_expression"],
        Language::Python => vec!["call"],
        Language::TypeScript | Language::Tsx => vec!["call_expression"],
        Language::Yaml => vec![],
    }
}

fn import_kinds(lang: Language) -> Vec<&'static str> {
    match lang {
        Language::Rust => vec!["use_declaration"],
        Language::Python => vec!["import_statement", "import_from_statement"],
        Language::TypeScript | Language::Tsx => vec!["import_statement"],
        Language::Yaml => vec![],
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
        if let Some(child) = node.child(i) {
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
        // Python: from X import a, b, c
        if let Some(after_import) = text.split("import").nth(1) {
            for part in after_import.split(',') {
                let name = part.trim().split(" as ").next().unwrap_or("").trim();
                if !name.is_empty() {
                    names.push(name.to_string());
                }
            }
        }
    } else if text.starts_with("import ") {
        // TS: import { X, Y } from './module'
        if text.contains('{') && text.contains('}') {
            if let Some(start) = text.find('{') {
                if let Some(end) = text.find('}') {
                    let inner = &text[start + 1..end];
                    for part in inner.split(',') {
                        let name = part.trim().split(" as ").next().unwrap_or("").trim();
                        if !name.is_empty() {
                            names.push(name.to_string());
                        }
                    }
                }
            }
            return names;
        }
        // Python: import sys
        let module = text.trim_start_matches("import ").trim();
        let name = module.split('.').last().unwrap_or(module).trim();
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
    seen: &mut std::collections::HashSet<String>,
) {
    let node_line = node.start_position().row as u32 + 1;

    if call_kinds.contains(&node.kind()) && node_line >= start_line && node_line <= end_line {
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
                        seen.insert(call_name.to_string());
                        break;
                    }
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            collect_calls_in_range(&child, source, _lang, call_kinds, start_line, end_line,
                all_funcs, out, seen);
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
        if let Some(child) = node.child(i) {
            collect_type_refs_in_range(&child, source, _lang, _type_kinds, start_line, end_line,
                all_types, out, seen);
        }
    }
}

fn collect_import_refs_in_range(
    _node: &tree_sitter::Node,
    _source: &str,
    _import_kinds: &[&str],
    start_line: u32,
    _end_line: u32,
    all_imports: &[(String, String)],
    out: &mut Vec<String>,
) {
    // Check which imports are referenced: for now, include all imports
    // whose imported name appears to be used. We simply include imports
    // that are in the file — a more precise approach would check for
    // identifier usage in the changed range, but for Phase 1 this suffices.
    // We include imports whose declaration line falls within the changed range,
    // OR whose imported name is used in the changed range.
    // For simplicity, include all file-level imports (they're context).
    let mut seen = std::collections::HashSet::new();
    for (name, text) in all_imports {
        if !seen.contains(name) {
            // Include the import if it provides context for the changed region
            // Simple heuristic: include all imports (they're cheap context)
            if start_line > 0 { // always true when we have changed lines
                out.push(format!("{}: {}", name, text.trim()));
                seen.insert(name.clone());
            }
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
                if let (Some(start), Some(count)) = (nums.first(), nums.get(1)) {
                    if let (Ok(s), Ok(c)) = (start.parse::<u32>(), count.parse::<u32>()) {
                        current_ranges.push((s, s + c.saturating_sub(1).max(0)));
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
        if let Some(child) = node.child(i) {
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
}
