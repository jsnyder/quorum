use crate::finding::{Finding, Severity, Source};
use crate::parser::{self, Language};

pub fn analyze_complexity(
    tree: &tree_sitter::Tree,
    source: &str,
    lang: Language,
    threshold: u32,
) -> Vec<Finding> {
    let threshold = threshold.max(1); // guard against 0
    let mut findings = Vec::new();

    let func_kinds = match lang {
        Language::Rust => &["function_item"][..],
        Language::Python => &["function_definition"][..],
        Language::TypeScript | Language::Tsx => &["function_declaration", "method_definition"][..],
    };

    let mut func_nodes = Vec::new();
    let mut cursor = tree.walk();
    collect_nodes_by_kind(&mut cursor, func_kinds, &mut func_nodes);

    for (start, end) in &func_nodes {
        let node = tree.root_node().descendant_for_byte_range(*start, *end);
        if let Some(node) = node {
            let cc = cyclomatic_complexity(&node, source, lang);
            if cc >= threshold {
                // Extract name directly from the function node
                let name = node
                    .child_by_field_name("name")
                    .map(|n| &source[n.byte_range()])
                    .unwrap_or("unknown");
                let severity = if cc >= threshold.saturating_mul(2) {
                    Severity::High
                } else {
                    Severity::Medium
                };
                findings.push(Finding {
                    title: format!("Function `{}` has cyclomatic complexity {}", name, cc),
                    description: format!(
                        "Cyclomatic complexity of {} exceeds threshold of {}. Consider refactoring.",
                        cc, threshold
                    ),
                    severity,
                    category: "complexity".into(),
                    source: Source::LocalAst,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    evidence: vec![format!("cyclomatic_complexity={}", cc)],
                    calibrator_action: None,
                    similar_precedent: vec![],
                });
            }
        }
    }
    findings
}

fn collect_nodes_by_kind(
    cursor: &mut tree_sitter::TreeCursor,
    kinds: &[&str],
    out: &mut Vec<(usize, usize)>,
) {
    let mut did_visit = false;
    loop {
        if !did_visit {
            let node = cursor.node();
            if kinds.contains(&node.kind()) {
                out.push((node.start_byte(), node.end_byte()));
            }
        }
        if !did_visit && cursor.goto_first_child() {
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            did_visit = true;
            continue;
        }
        break;
    }
}

pub fn cyclomatic_complexity(
    node: &tree_sitter::Node,
    source: &str,
    lang: Language,
) -> u32 {
    let mut complexity = 1u32; // baseline path
    count_decisions(node, source, lang, &mut complexity);
    complexity
}

fn count_decisions(
    node: &tree_sitter::Node,
    source: &str,
    lang: Language,
    complexity: &mut u32,
) {
    let kind = node.kind();

    match kind {
        // Branching
        "if_expression" | "if_statement" | "if_let_expression" => *complexity += 1,
        "elif_clause" | "else_if_clause" => *complexity += 1,

        // Loops
        "for_expression" | "for_statement" | "for_in_statement" => *complexity += 1,
        "while_expression" | "while_statement" => *complexity += 1,

        // Match/switch arms (each arm is a path)
        "match_arm" | "case_clause" | "default_clause" => *complexity += 1,

        // Exception handling
        "except_clause" | "catch_clause" => *complexity += 1,

        // Ternary
        "ternary_expression" | "conditional_expression" => *complexity += 1,

        // Logical operators (short-circuit = decision point)
        "binary_expression" => {
            if let Some(op_node) = node.child_by_field_name("operator") {
                let op = &source[op_node.byte_range()];
                if op == "&&" || op == "||" || op == "and" || op == "or" {
                    *complexity += 1;
                }
            }
        }

        _ => {}
    }

    // Recurse into children
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            count_decisions(&child, source, lang, complexity);
        }
    }
}

pub fn analyze_insecure_patterns(
    tree: &tree_sitter::Tree,
    source: &str,
    lang: Language,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    scan_insecure_nodes(&tree.root_node(), source, lang, &mut findings);
    findings
}

fn scan_insecure_nodes(
    node: &tree_sitter::Node,
    source: &str,
    lang: Language,
    findings: &mut Vec<Finding>,
) {
    match lang {
        Language::Rust => scan_insecure_rust(node, source, findings),
        Language::Python => scan_insecure_python(node, source, findings),
        Language::TypeScript | Language::Tsx => scan_insecure_typescript(node, source, findings),
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            scan_insecure_nodes(&child, source, lang, findings);
        }
    }
}

/// Check if a node is inside a test context: #[cfg(test)] module or #[test] function.
fn is_in_test_context(node: &tree_sitter::Node, source: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        // Check for #[cfg(test)] on mod items
        if parent.kind() == "mod_item" {
            if has_attribute(&parent, source, "cfg(test)") {
                return true;
            }
        }
        // Check for #[test] on function items
        if parent.kind() == "function_item" {
            if has_attribute(&parent, source, "test") {
                return true;
            }
        }
        current = parent.parent();
    }
    false
}

fn has_attribute(node: &tree_sitter::Node, source: &str, attr_name: &str) -> bool {
    // Walk siblings before this node looking for attribute_item nodes
    let mut sibling = node.prev_sibling();
    while let Some(s) = sibling {
        if s.kind() == "attribute_item" || s.kind() == "inner_attribute_item" {
            let text = &source[s.byte_range()];
            if text.contains(attr_name) {
                return true;
            }
        } else {
            // Stop looking once we hit a non-attribute node
            break;
        }
        sibling = s.prev_sibling();
    }
    false
}

fn scan_insecure_rust(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    // unsafe blocks
    if node.kind() == "unsafe_block" {
        findings.push(Finding {
            title: "Use of `unsafe` block".into(),
            description: "Unsafe code bypasses Rust's safety guarantees. Ensure this is necessary and correct.".into(),
            severity: Severity::Info,
            category: "security".into(),
            source: Source::LocalAst,
            line_start: node.start_position().row as u32 + 1,
            line_end: node.end_position().row as u32 + 1,
            evidence: vec![source[node.byte_range()].chars().take(200).collect()],
            calibrator_action: None,
            similar_precedent: vec![],
        });
    }

    // .unwrap() calls — tree-sitter-rust: call_expression with function=field_expression
    // Skip unwrap in test code (#[cfg(test)] modules, #[test] functions)
    if node.kind() == "call_expression" && !is_in_test_context(node, source) {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "field_expression" {
                if let Some(field) = func.child_by_field_name("field") {
                    let field_name = &source[field.byte_range()];
                    if field_name == "unwrap" {
                        findings.push(Finding {
                            title: "Use of `.unwrap()` may panic at runtime".into(),
                            description: "Consider using `.expect()` with a message or proper error handling.".into(),
                            severity: Severity::Low,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: node.start_position().row as u32 + 1,
                            line_end: node.end_position().row as u32 + 1,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                        });
                    }
                }
            }
        }
    }
}

fn scan_insecure_python(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    // eval() and exec() calls
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            let func_name = &source[func.byte_range()];
            if func_name == "eval" || func_name == "exec" {
                findings.push(Finding {
                    title: format!("Use of `{}()` is a code injection risk", func_name),
                    description: format!(
                        "`{}()` executes arbitrary code. Avoid using it with untrusted input.",
                        func_name
                    ),
                    severity: Severity::Critical,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                });
            }
        }
    }
}

fn scan_insecure_typescript(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    // eval() calls
    if node.kind() == "call_expression" {
        if let Some(func) = node.child_by_field_name("function") {
            let func_name = &source[func.byte_range()];
            if func_name == "eval" {
                findings.push(Finding {
                    title: "Use of `eval()` is a code injection risk".into(),
                    description: "`eval()` executes arbitrary code. Avoid using it with untrusted input.".into(),
                    severity: Severity::Critical,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    // -- Cyclomatic complexity scoring --

    #[test]
    fn complexity_baseline_linear_function_rust() {
        let source = "fn simple() -> i32 { 42 }";
        let tree = parse(source, Language::Rust).unwrap();
        let root = tree.root_node();
        let func = root.child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 1);
    }

    #[test]
    fn complexity_single_if_rust() {
        let source = "fn check(x: bool) { if x { return; } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 2);
    }

    #[test]
    fn complexity_if_else_rust() {
        let source = "fn check(x: bool) { if x { 1 } else { 2 } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        // if adds 1 path, else doesn't add (it's the other branch of existing decision)
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 2);
    }

    #[test]
    fn complexity_nested_ifs_rust() {
        let source = "fn nested(a: bool, b: bool) { if a { if b { return; } } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 3);
    }

    #[test]
    fn complexity_match_arms_rust() {
        let source = r#"fn dispatch(x: i32) { match x { 1 => {}, 2 => {}, 3 => {}, _ => {} } }"#;
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        // 4 match arms = base(1) + 4 arms = 5
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 5);
    }

    #[test]
    fn complexity_for_loop_rust() {
        let source = "fn loopy(items: &[i32]) { for x in items { println!(\"{}\", x); } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 2);
    }

    #[test]
    fn complexity_while_loop_rust() {
        let source = "fn loopy() { while true { break; } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 2);
    }

    #[test]
    fn complexity_logical_operators_rust() {
        let source = "fn check(a: bool, b: bool, c: bool) { if a && b || c { return; } }";
        let tree = parse(source, Language::Rust).unwrap();
        let func = tree.root_node().child(0).unwrap();
        // if=1, &&=1, ||=1, base=1 => 4
        assert_eq!(cyclomatic_complexity(&func, source, Language::Rust), 4);
    }

    // -- Python --

    #[test]
    fn complexity_baseline_python() {
        let source = "def simple():\n    return 42\n";
        let tree = parse(source, Language::Python).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::Python), 1);
    }

    #[test]
    fn complexity_if_elif_python() {
        let source = "def check(x):\n    if x > 0:\n        return 1\n    elif x < 0:\n        return -1\n    else:\n        return 0\n";
        let tree = parse(source, Language::Python).unwrap();
        let func = tree.root_node().child(0).unwrap();
        // if + elif = 3
        assert_eq!(cyclomatic_complexity(&func, source, Language::Python), 3);
    }

    // -- TypeScript --

    #[test]
    fn complexity_baseline_typescript() {
        let source = "function simple() { return 42; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::TypeScript), 1);
    }

    #[test]
    fn complexity_ternary_typescript() {
        let source = "function check(x: boolean) { return x ? 1 : 0; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let func = tree.root_node().child(0).unwrap();
        assert_eq!(cyclomatic_complexity(&func, source, Language::TypeScript), 2);
    }

    // -- analyze_complexity integration --

    #[test]
    fn analyze_flags_complex_function() {
        let source = "fn complex(a: bool, b: bool, c: bool, d: bool) {\n    if a {\n        if b {\n            if c {\n                if d {\n                    return;\n                }\n            }\n        }\n    }\n    if a && b {\n        return;\n    }\n    for x in 0..10 {\n        if x > 5 {\n            break;\n        }\n    }\n}\n";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_complexity(&tree, source, Language::Rust, 5);
        assert!(!findings.is_empty(), "complex function should produce a finding");
        assert_eq!(findings[0].source, Source::LocalAst);
        assert_eq!(findings[0].category, "complexity");
        assert!(findings[0].severity >= Severity::Medium);
    }

    #[test]
    fn analyze_ignores_simple_functions() {
        let source = "fn simple() -> i32 { 42 }\nfn also_simple(x: bool) { if x { return; } }";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_complexity(&tree, source, Language::Rust, 5);
        assert!(findings.is_empty(), "simple functions should not produce findings");
    }

    #[test]
    fn analyze_complexity_findings_have_valid_line_ranges() {
        let source = "fn complex(a: bool, b: bool) {\n    if a {\n        if b {\n            for i in 0..10 {\n                if i > 5 {\n                    while true {\n                        break;\n                    }\n                }\n            }\n        }\n    }\n}\n";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_complexity(&tree, source, Language::Rust, 3);
        for f in &findings {
            assert!(f.is_valid());
            assert!(f.line_start >= 1);
        }
    }

    // -- Insecure pattern detection --

    #[test]
    fn insecure_eval_python() {
        let source = "def run(code):\n    result = eval(code)\n    return result\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(!findings.is_empty());
        assert_eq!(findings[0].category, "security");
        assert!(findings[0].title.contains("eval"));
    }

    #[test]
    fn insecure_exec_python() {
        let source = "def run(code):\n    exec(code)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(!findings.is_empty());
        assert!(findings[0].title.contains("exec"));
    }

    #[test]
    fn insecure_eval_typescript() {
        let source = "function run(code: string) { return eval(code); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(!findings.is_empty());
        assert!(findings[0].title.contains("eval"));
    }

    #[test]
    fn insecure_unsafe_rust() {
        let source = "fn dangerous() { unsafe { std::ptr::null::<i32>().read() }; }";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(!findings.is_empty());
        assert!(findings[0].title.contains("unsafe"));
        // unsafe is info severity, not critical
        assert_eq!(findings[0].severity, Severity::Info);
    }

    #[test]
    fn insecure_unwrap_rust() {
        let source = "fn risky(x: Option<i32>) -> i32 { x.unwrap() }";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(findings.iter().any(|f| f.title.contains("unwrap")));
    }

    #[test]
    fn safe_code_no_findings_python() {
        let source = "def safe():\n    return 42\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(findings.is_empty());
    }

    #[test]
    fn safe_code_no_findings_rust() {
        let source = "fn safe() -> i32 { 42 }";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(findings.is_empty());
    }

    #[test]
    fn insecure_findings_tagged_local_ast() {
        let source = "def run(code):\n    eval(code)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        for f in &findings {
            assert_eq!(f.source, Source::LocalAst);
        }
    }

    // -- Test code filtering --

    #[test]
    fn unwrap_in_test_module_filtered() {
        let source = r#"
fn prod() -> i32 { 42 }

#[cfg(test)]
mod tests {
    fn test_helper() {
        let x: Option<i32> = Some(1);
        x.unwrap();
    }
}
"#;
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        // unwrap inside #[cfg(test)] module should be filtered
        assert!(
            !findings.iter().any(|f| f.title.contains("unwrap")),
            "unwrap in test module should be filtered. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn unwrap_in_test_function_filtered() {
        let source = r#"
#[test]
fn my_test() {
    let x: Option<i32> = Some(1);
    x.unwrap();
}
"#;
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(
            !findings.iter().any(|f| f.title.contains("unwrap")),
            "unwrap in #[test] function should be filtered"
        );
    }

    #[test]
    fn unwrap_in_prod_code_still_flagged() {
        let source = "fn risky(x: Option<i32>) -> i32 { x.unwrap() }";
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(findings.iter().any(|f| f.title.contains("unwrap")));
    }

    #[test]
    fn unsafe_in_test_module_still_flagged() {
        // unsafe is always worth noting, even in tests
        let source = r#"
#[cfg(test)]
mod tests {
    fn test_unsafe() {
        unsafe { std::ptr::null::<i32>().read() };
    }
}
"#;
        let tree = parse(source, Language::Rust).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Rust);
        assert!(findings.iter().any(|f| f.title.contains("unsafe")));
    }
}
