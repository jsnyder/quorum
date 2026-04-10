use crate::finding::{Finding, Severity, Source};
use crate::parser::Language;

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
        Language::Yaml => &[][..],
        Language::Bash => &[][..],
        Language::Dockerfile => &[][..],
        Language::Terraform => &[][..],
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
                    canonical_pattern: None,
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
    if matches!(lang, Language::Dockerfile) {
        findings.extend(analyze_dockerfile_structure(tree, source));
    }
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
        Language::Yaml => scan_insecure_yaml(node, source, findings),
        Language::Bash => scan_insecure_bash(node, source, findings),
        Language::Dockerfile => scan_insecure_dockerfile(node, source, findings),
        Language::Terraform => {},
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
            canonical_pattern: None,
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
                            canonical_pattern: None,
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
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // eval() and exec() calls (also covered by ruff S307, but we keep for standalone mode)
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
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }

        // debug=True or host="0.0.0.0" in function calls (Flask/FastAPI/uvicorn)
        if let Some(args) = node.child_by_field_name("arguments") {
            let args_text = &source[args.byte_range()];
            if args_text.contains("debug=True") || args_text.contains("debug = True") {
                findings.push(Finding {
                    title: "Server running with debug=True".into(),
                    description: "Debug mode exposes detailed error pages and may enable a debugger. Disable in production.".into(),
                    severity: Severity::High,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
            if args_text.contains("host=\"0.0.0.0\"") || args_text.contains("host='0.0.0.0'") {
                findings.push(Finding {
                    title: "Server binding to 0.0.0.0 exposes all network interfaces".into(),
                    description: "Binding to 0.0.0.0 makes the server accessible from any network interface. Use 127.0.0.1 for local-only access.".into(),
                    severity: Severity::Medium,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }

        // SQL injection: .execute() with f-string or .format() argument
        if let Some(func) = node.child_by_field_name("function") {
            let func_text = &source[func.byte_range()];
            if func_text.ends_with(".execute") || func_text.ends_with(".executemany") {
                if let Some(args) = node.child_by_field_name("arguments") {
                    // Check first argument for f-string or .format()
                    if let Some(first_arg) = args.named_child(0) {
                        let arg_kind = first_arg.kind();
                        let arg_text = &source[first_arg.byte_range()];
                        if arg_kind == "string"
                            && (arg_text.starts_with("f\"") || arg_text.starts_with("f'"))
                        {
                            findings.push(Finding {
                                title: "Potential SQL injection via f-string in execute()".into(),
                                description: "String interpolation in SQL queries allows injection. Use parameterized queries instead.".into(),
                                severity: Severity::Critical,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        } else if arg_text.contains(".format(") {
                            findings.push(Finding {
                                title: "Potential SQL injection via .format() in execute()".into(),
                                description: "String formatting in SQL queries allows injection. Use parameterized queries instead.".into(),
                                severity: Severity::Critical,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }
        }

        // open() without explicit encoding
        if let Some(func) = node.child_by_field_name("function") {
            let func_name = &source[func.byte_range()];
            if func_name == "open" {
                if let Some(args) = node.child_by_field_name("arguments") {
                    let args_text = &source[args.byte_range()];
                    let binary_modes = [
                        "'rb'", "\"rb\"", "'wb'", "\"wb\"", "'ab'", "\"ab\"", "'xb'", "\"xb\"",
                        "'r+b'", "\"r+b\"", "'w+b'", "\"w+b\"", "'a+b'", "\"a+b\"", "'x+b'", "\"x+b\"",
                        "'rb+'", "\"rb+\"", "'wb+'", "\"wb+\"", "'ab+'", "\"ab+\"", "'xb+'", "\"xb+\"",
                    ];
                    let is_binary = binary_modes.iter().any(|m| args_text.contains(m));
                    let has_encoding = args_text.contains("encoding=")
                        || args_text.contains("encoding =");
                    if !is_binary && !has_encoding {
                        findings.push(Finding {
                            title: "`open()` without explicit `encoding` parameter".into(),
                            description: "Without `encoding=`, open() uses the system default which varies by platform. Specify `encoding='utf-8'` for portable behavior.".into(),
                            severity: Severity::Low,
                            category: "reliability".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // Bare except: pass (catch-all that silently swallows)
    if node.kind() == "except_clause" {
        // Determine exception type and body by walking children.
        // tree-sitter-python except_clause children: optional exception type/as_pattern, then block.
        let mut exception_type: Option<String> = None;
        let mut body_node: Option<tree_sitter::Node> = None;
        for i in 0..node.named_child_count() {
            if let Some(child) = node.named_child(i) {
                match child.kind() {
                    "identifier" => {
                        exception_type = Some(source[child.byte_range()].to_string());
                    }
                    "as_pattern" => {
                        // `except Exception as e` — extract the type from the first child
                        if let Some(type_child) = child.named_child(0) {
                            exception_type = Some(source[type_child.byte_range()].to_string());
                        }
                    }
                    "block" => {
                        body_node = Some(child);
                    }
                    _ => {}
                }
            }
        }
        let is_catch_all = match &exception_type {
            None => true, // bare `except:`
            Some(t) => t == "Exception" || t == "BaseException",
        };
        if is_catch_all {
            if let Some(body) = body_node {
                let body_has_only_pass = body.named_child_count() == 1
                    && body.named_child(0).map(|c| c.kind()) == Some("pass_statement");
                if body_has_only_pass {
                    findings.push(Finding {
                        title: "Catch-all `except: pass` silently swallows errors".into(),
                        description: "Catching all exceptions with `pass` hides bugs. Log the error or catch a specific exception type.".into(),
                        severity: Severity::Medium,
                        category: "reliability".into(),
                        source: Source::LocalAst,
                        line_start: line,
                        line_end: end_line,
                        evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                        calibrator_action: None,
                        similar_precedent: vec![],
                        canonical_pattern: None,
                    });
                }
            }
        }
    }

    // Hardcoded secrets: SECRET_KEY = "...", PASSWORD = "...", API_KEY = "..."
    if node.kind() == "assignment" {
        if let Some(left) = node.child_by_field_name("left") {
            let var_name = source[left.byte_range()].to_uppercase();
            let secret_names = [
                "SECRET_KEY", "SECRET", "PASSWORD", "PASSWD", "API_KEY",
                "APIKEY", "AUTH_TOKEN", "TOKEN", "PRIVATE_KEY",
            ];
            if secret_names.iter().any(|s| var_name.contains(s)) {
                if let Some(right) = node.child_by_field_name("right") {
                    let right_kind = right.kind();
                    let right_text = &source[right.byte_range()];
                    // Only flag string literals that look like real secrets:
                    // - Not empty, not None, not env lookups
                    // - Longer than a typical key name (> 10 chars inside quotes)
                    // - Contains mixed case, numbers, or special chars (not just lowercase words)
                    // Guard: only slice into string content if it's actually a quoted string
                    let inner_len = if right_text.len() > 2 { right_text.len() - 2 } else { 0 };
                    let inner = if right_text.len() > 2 {
                        &right_text[1..right_text.len() - 1]
                    } else {
                        ""
                    };
                    let has_upper = inner.chars().any(|c| c.is_ascii_uppercase());
                    let has_digit = inner.chars().any(|c| c.is_ascii_digit());
                    let has_special = inner.chars().any(|c| matches!(c, '-' | '/' | '+' | '='));
                    // Real secrets have mixed character classes (upper+lower, digits, special chars)
                    // Plain lowercase_words or dotted.names are key names, not secrets
                    let looks_like_secret = (has_upper || has_digit || has_special)
                        && inner_len > 8;
                    if (right_kind == "string" || right_kind == "concatenated_string")
                        && inner_len > 3
                        && looks_like_secret
                        && !right_text.contains("os.environ")
                        && !right_text.contains("getenv")
                        && !inner.starts_with("http://")
                        && !inner.starts_with("https://")
                    {
                        findings.push(Finding {
                            title: format!("Hardcoded secret in `{}`", &source[left.byte_range()]),
                            description: "Secrets should be loaded from environment variables or a secrets manager, not hardcoded in source.".into(),
                            severity: Severity::High,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![format!("{} = [REDACTED]", &source[left.byte_range()])],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // Mutating collection while iterating
    if node.kind() == "for_statement" {
        // Get the iterable: `for x in ITERABLE:`
        // In tree-sitter-python, for_statement has fields: left (pattern), right (iterable), body
        if let Some(right) = node.child_by_field_name("right") {
            // Only match when iterating directly over an identifier (not list(items) or other call)
            if right.kind() == "identifier" {
                let iterable_name = &source[right.byte_range()];
                if let Some(body) = node.child_by_field_name("body") {
                    if has_mutating_call(&body, source, iterable_name) {
                        findings.push(Finding {
                            title: format!("Mutating `{}` while iterating over it", iterable_name),
                            description: "Modifying a collection while iterating over it leads to skipped elements or RuntimeError. Iterate over a copy instead.".into(),
                            severity: Severity::High,
                            category: "bug".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // Exception details in API response
    if node.kind() == "except_clause" {
        // Find the exception variable name from `as_pattern` child
        // Tree structure: except_clause > as_pattern > as_pattern_target > identifier
        let mut exc_var: Option<&str> = None;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "as_pattern" {
                    // Look for as_pattern_target child which contains the identifier
                    for j in 0..child.child_count() {
                        if let Some(target) = child.child(j) {
                            if target.kind() == "as_pattern_target" {
                                if let Some(ident) = target.child(0) {
                                    if ident.kind() == "identifier" {
                                        exc_var = Some(&source[ident.byte_range()]);
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }
        if let Some(var_name) = exc_var {
            // Look for return statements that expose the exception
            if has_exception_in_return(node, source, var_name) {
                findings.push(Finding {
                    title: "Exception details disclosed in API response".into(),
                    description: format!(
                        "Returning `str({0})` or `repr({0})` in an API response leaks internal details to clients. Log the exception and return a generic error message.",
                        var_name
                    ),
                    severity: Severity::Medium,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // Blocking .result() in async function
    if node.kind() == "function_definition" {
        // Check if this function is async by looking for the "async" keyword
        // In tree-sitter-python, async functions have "async" as a preceding sibling or
        // the node text starts with "async"
        let func_text = &source[node.byte_range()];
        if func_text.starts_with("async ") {
            if has_result_call(node, source) {
                findings.push(Finding {
                    title: "Blocking `.result()` call in async function".into(),
                    description: "Calling `.result()` on a future inside an async function blocks the event loop. Use `await` or run in an executor.".into(),
                    severity: Severity::High,
                    category: "concurrency".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // Mutable default arguments: def foo(x=[], y={})
    if node.kind() == "default_parameter" {
        if let Some(value) = node.child_by_field_name("value") {
            let val_kind = value.kind();
            if val_kind == "list" || val_kind == "dictionary" || val_kind == "set" {
                let param_name = node
                    .child_by_field_name("name")
                    .map(|n| &source[n.byte_range()])
                    .unwrap_or("parameter");
                findings.push(Finding {
                    title: format!("Mutable default argument `{}`", param_name),
                    description: "Mutable default arguments are shared across calls and cause subtle bugs. Use None and initialize inside the function.".into(),
                    severity: Severity::Medium,
                    category: "bug".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(100).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }
}

/// Check if a node tree contains a mutating method call on the given identifier.
fn has_mutating_call(node: &tree_sitter::Node, source: &str, target: &str) -> bool {
    let mutating_methods = ["append", "remove", "pop", "insert", "extend", "clear"];

    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "attribute" {
                if let Some(obj) = func.child_by_field_name("object") {
                    if obj.kind() == "identifier" && &source[obj.byte_range()] == target {
                        if let Some(attr) = func.child_by_field_name("attribute") {
                            let method = &source[attr.byte_range()];
                            if mutating_methods.contains(&method) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if has_mutating_call(&child, source, target) {
                return true;
            }
        }
    }
    false
}

/// Check if an except_clause contains a return statement that exposes the exception variable.
fn has_exception_in_return(node: &tree_sitter::Node, source: &str, var_name: &str) -> bool {
    if node.kind() == "return_statement" {
        let text = &source[node.byte_range()];
        let str_pattern = format!("str({})", var_name);
        let repr_pattern = format!("repr({})", var_name);
        // Also check for f-string interpolation like {e} or {e!r}
        let fstring_pattern = format!("{{{}}}", var_name);
        if text.contains(&str_pattern) || text.contains(&repr_pattern) || text.contains(&fstring_pattern) {
            return true;
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if has_exception_in_return(&child, source, var_name) {
                return true;
            }
        }
    }
    false
}

/// Check if a function body contains a .result() call.
fn has_result_call(node: &tree_sitter::Node, source: &str) -> bool {
    if node.kind() == "call" {
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "attribute" {
                if let Some(attr) = func.child_by_field_name("attribute") {
                    if &source[attr.byte_range()] == "result" {
                        return true;
                    }
                }
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if has_result_call(&child, source) {
                return true;
            }
        }
    }
    false
}

/// Check if a node is inside an async function by inspecting AST structure.
/// Looks for an "async" child token on function_declaration, method_definition, or arrow_function.
fn is_in_async_function(node: &tree_sitter::Node, _source: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "function_declaration" | "function_expression" | "method_definition" | "function" | "arrow_function" => {
                let mut cursor = parent.walk();
                for child in parent.children(&mut cursor) {
                    if child.kind() == "async" {
                        return true;
                    }
                }
                return false;
            }
            _ => {}
        }
        current = parent.parent();
    }
    false
}

// ---------------------------------------------------------------------------
// YAML / Home Assistant patterns
// ---------------------------------------------------------------------------

/// Secret-like key name patterns for YAML hardcoded secret detection.
const YAML_SECRET_KEY_PATTERNS: &[&str] = &[
    "password", "passwd", "secret", "api_key", "apikey", "auth_token",
    "token", "private_key", "access_key", "secret_key",
];

/// Check whether the value side of a block_mapping_pair uses a safe tag
/// (!secret, !include, !env_var).
fn yaml_value_has_safe_tag(value_node: &tree_sitter::Node, source: &str) -> bool {
    let text = &source[value_node.byte_range()];
    if text.starts_with("!secret") || text.starts_with("!include") || text.starts_with("!env_var") {
        return true;
    }
    // The value might be wrapped in a flow_node or block_node that contains a tag child.
    for i in 0..value_node.child_count() {
        if let Some(child) = value_node.child(i) {
            if child.kind() == "tag" {
                let tag_text = &source[child.byte_range()];
                if tag_text.starts_with("!secret") || tag_text.starts_with("!include") || tag_text.starts_with("!env_var") {
                    return true;
                }
            }
            // Recurse one more level (flow_node may contain tag)
            for j in 0..child.child_count() {
                if let Some(gc) = child.child(j) {
                    if gc.kind() == "tag" {
                        let tag_text = &source[gc.byte_range()];
                        if tag_text.starts_with("!secret") || tag_text.starts_with("!include") || tag_text.starts_with("!env_var") {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Check if a `block_mapping` node is the *direct* mapping of an automation
/// list item (i.e. the immediate child of a block_sequence_item under an
/// `automation:` key). Nested mappings inside that item return false.
fn is_in_automation_context(node: &tree_sitter::Node, source: &str) -> bool {
    // Expect: block_mapping -> (block_node?) -> block_sequence_item -> block_sequence -> (block_node?) -> block_mapping_pair[key="automation"]
    let mut parent = match node.parent() {
        Some(p) => p,
        None => return false,
    };
    // Skip optional block_node wrapper
    if parent.kind() == "block_node" {
        parent = match parent.parent() {
            Some(p) => p,
            None => return false,
        };
    }
    if parent.kind() != "block_sequence_item" {
        return false;
    }
    let seq = match parent.parent() {
        Some(p) if p.kind() == "block_sequence" => p,
        _ => return false,
    };
    let mut candidate = seq.parent();
    // Skip through block_node wrappers
    while let Some(c) = candidate {
        if c.kind() == "block_node" {
            candidate = c.parent();
            continue;
        }
        if c.kind() == "block_mapping_pair" {
            if let Some(key) = c.child_by_field_name("key") {
                let key_text = source[key.byte_range()].trim();
                return key_text == "automation" || key_text == "automation!";
            }
        }
        break;
    }
    false
}

/// Collect the set of keys present in a block_mapping node.
fn collect_mapping_keys<'a>(node: &tree_sitter::Node, source: &'a str) -> Vec<&'a str> {
    let mut keys = Vec::new();
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "block_mapping_pair" {
                if let Some(key) = child.child_by_field_name("key") {
                    keys.push(source[key.byte_range()].trim());
                }
            }
        }
    }
    keys
}

/// Check if a block_mapping_pair's value is empty (no meaningful children).
fn yaml_value_is_empty(pair_node: &tree_sitter::Node, source: &str) -> bool {
    if let Some(value) = pair_node.child_by_field_name("value") {
        let text = source[value.byte_range()].trim();
        if text.is_empty() {
            return true;
        }
        // Check if value has a block_sequence child with items
        for i in 0..value.child_count() {
            if let Some(child) = value.child(i) {
                if child.kind() == "block_sequence" {
                    let mut has_items = false;
                    for j in 0..child.child_count() {
                        if let Some(item) = child.child(j) {
                            if item.kind() == "block_sequence_item" {
                                has_items = true;
                                break;
                            }
                        }
                    }
                    return !has_items;
                }
            }
        }
        false
    } else {
        true
    }
}

/// Check if a URL string contains embedded credentials (e.g. `://user:pass@`).
fn yaml_url_has_credentials(url: &str) -> bool {
    // Find the `://` marker, then check the authority section before the next `/`
    if let Some(scheme_end) = url.find("://") {
        let after_scheme = &url[scheme_end + 3..];
        // The authority section ends at the first `/` (or end of string)
        let authority = if let Some(slash) = after_scheme.find('/') {
            &after_scheme[..slash]
        } else {
            after_scheme
        };
        // Credentials pattern: something@host, where the part before @ contains `:`
        if let Some(at_pos) = authority.find('@') {
            let userinfo = &authority[..at_pos];
            return userinfo.contains(':');
        }
    }
    false
}

/// Check if a block_mapping is at the document root level.
fn is_root_mapping(node: &tree_sitter::Node) -> bool {
    // Expected: block_mapping -> block_node -> document (or stream -> document)
    let mut current = node.parent();
    while let Some(parent) = current {
        match parent.kind() {
            "block_node" => { current = parent.parent(); continue; }
            "document" | "stream" => return true,
            _ => return false,
        }
    }
    false
}

/// Find a block_mapping_pair by key name in a block_mapping, and return
/// the value node's child block_mapping (if it has one).
fn find_value_mapping<'a>(
    mapping_node: &'a tree_sitter::Node<'a>,
    source: &str,
    key_name: &str,
) -> Option<tree_sitter::Node<'a>> {
    for i in 0..mapping_node.child_count() {
        if let Some(child) = mapping_node.child(i) {
            if child.kind() == "block_mapping_pair" {
                if let Some(key) = child.child_by_field_name("key") {
                    if source[key.byte_range()].trim() == key_name {
                        if let Some(value) = child.child_by_field_name("value") {
                            // Walk through block_node wrappers to find block_mapping
                            return find_block_mapping_in(&value);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Recursively find a block_mapping inside a value node (through block_node wrappers).
fn find_block_mapping_in<'a>(node: &tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
    if node.kind() == "block_mapping" {
        return Some(*node);
    }
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "block_mapping" {
                return Some(child);
            }
            if child.kind() == "block_node" {
                if let Some(found) = find_block_mapping_in(&child) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn scan_insecure_yaml(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // --- Tier 1: Duplicate keys in a block_mapping ---
    if node.kind() == "block_mapping" {
        let mut seen_keys: Vec<(&str, u32)> = Vec::new();
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "block_mapping_pair" {
                    if let Some(key) = child.child_by_field_name("key") {
                        let key_text = source[key.byte_range()].trim();
                        let key_line = key.start_position().row as u32 + 1;
                        if let Some((_, first_line)) = seen_keys.iter().find(|(k, _)| *k == key_text) {
                            findings.push(Finding {
                                title: format!("Duplicate key `{}` in mapping", key_text),
                                description: format!(
                                    "Key `{}` appears multiple times in the same mapping (first at line {}). The last value silently wins.",
                                    key_text, first_line
                                ),
                                severity: Severity::High,
                                category: "bug".into(),
                                source: Source::LocalAst,
                                line_start: key_line,
                                line_end: key_line,
                                evidence: vec![],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        } else {
                            seen_keys.push((key_text, key_line));
                        }
                    }
                }
            }
        }

        // --- Tier 2: Automation-level checks ---
        if is_in_automation_context(node, source) {
            let keys = collect_mapping_keys(node, source);

            // 3. Missing id
            if !keys.contains(&"id") {
                findings.push(Finding {
                    title: "Automation missing `id` -- UI management and debug traces are disabled".into(),
                    description: "Automation missing `id` -- UI management and debug traces are disabled".into(),
                    severity: Severity::Medium,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }

            // 4. Missing mode
            if !keys.contains(&"mode") {
                findings.push(Finding {
                    title: "Automation has no explicit `mode` (defaults to `single`)".into(),
                    description: "Automation has no explicit `mode` (defaults to `single`)".into(),
                    severity: Severity::Info,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }

            // 5. Deprecated singular trigger/action/condition
            let deprecated = [
                ("trigger", "triggers"),
                ("action", "actions"),
                ("condition", "conditions"),
            ];
            for (singular, plural) in &deprecated {
                if keys.contains(singular) && !keys.contains(plural) {
                    findings.push(Finding {
                        title: format!("Deprecated singular `{}:` -- use `{}:` instead", singular, plural),
                        description: format!(
                            "Home Assistant deprecated the singular `{}:` key. Use `{}:` (plural) for forward compatibility.",
                            singular, plural
                        ),
                        severity: Severity::Medium,
                        category: "quality".into(),
                        source: Source::LocalAst,
                        line_start: line,
                        line_end: end_line,
                        evidence: vec![],
                        calibrator_action: None,
                        similar_precedent: vec![],
                        canonical_pattern: None,
                    });
                }
            }

            // 6. Empty triggers or actions
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i) {
                    if child.kind() == "block_mapping_pair" {
                        if let Some(key) = child.child_by_field_name("key") {
                            let key_text = source[key.byte_range()].trim();
                            if (key_text == "triggers" || key_text == "actions") && yaml_value_is_empty(&child, source) {
                                findings.push(Finding {
                                    title: format!("Empty `{}:` in automation", key_text),
                                    description: format!(
                                        "The `{}:` key has no items. This automation will not function correctly.",
                                        key_text
                                    ),
                                    severity: Severity::High,
                                    category: "bug".into(),
                                    source: Source::LocalAst,
                                    line_start: child.start_position().row as u32 + 1,
                                    line_end: child.end_position().row as u32 + 1,
                                    evidence: vec![],
                                    calibrator_action: None,
                                    similar_precedent: vec![],
                                    canonical_pattern: None,
                                });
                            }
                        }
                    }
                }
            }
        }

        // --- Tier 5: ESPHome-specific checks (root-level mapping only) ---
        if is_root_mapping(node) {
            let keys = collect_mapping_keys(node, source);

            if keys.contains(&"esphome") {
                // Pattern 17: ESPHome OTA without password
                if keys.contains(&"ota") {
                    let has_password = if let Some(ota_mapping) = find_value_mapping(node, source, "ota") {
                        let ota_keys = collect_mapping_keys(&ota_mapping, source);
                        ota_keys.contains(&"password")
                    } else {
                        false
                    };
                    if !has_password {
                        findings.push(Finding {
                            title: "ESPHome OTA section has no password -- firmware updates are unprotected".into(),
                            description: "ESPHome OTA section has no password -- firmware updates are unprotected. Add a password to prevent unauthorized firmware uploads.".into(),
                            severity: Severity::Medium,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }

                // Pattern 19: ESPHome API without encryption
                if keys.contains(&"api") {
                    let has_encryption = if let Some(api_mapping) = find_value_mapping(node, source, "api") {
                        let api_keys = collect_mapping_keys(&api_mapping, source);
                        api_keys.contains(&"encryption")
                    } else {
                        false
                    };
                    if !has_encryption {
                        findings.push(Finding {
                            title: "ESPHome API has no encryption configured".into(),
                            description: "ESPHome API has no encryption configured. Add an encryption key to secure communication.".into(),
                            severity: Severity::Medium,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // --- Tier 1: Hardcoded secrets (on block_mapping_pair nodes) ---
    if node.kind() == "block_mapping_pair" {
        if let Some(key) = node.child_by_field_name("key") {
            let key_text = source[key.byte_range()].trim().to_lowercase();

            if YAML_SECRET_KEY_PATTERNS.iter().any(|p| key_text.contains(p)) {
                if let Some(value) = node.child_by_field_name("value") {
                    if !yaml_value_has_safe_tag(&value, source) {
                        let val_text = source[value.byte_range()].trim();
                        if !val_text.is_empty() {
                            findings.push(Finding {
                                title: format!("Hardcoded secret in `{}`", source[key.byte_range()].trim()),
                                description: "Secrets should use `!secret` references, not hardcoded values.".into(),
                                severity: Severity::High,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![format!("{}: [REDACTED]", source[key.byte_range()].trim())],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }

            // --- Tier 3: entity_id without domain ---
            if key_text == "entity_id" {
                if let Some(value) = node.child_by_field_name("value") {
                    let val_text = source[value.byte_range()].trim();
                    let mut is_list = false;
                    for i in 0..value.child_count() {
                        if let Some(child) = value.child(i) {
                            if child.kind() == "block_sequence" {
                                is_list = true;
                                for j in 0..child.child_count() {
                                    if let Some(item) = child.child(j) {
                                        if item.kind() == "block_sequence_item" {
                                            let item_text = source[item.byte_range()].trim().trim_start_matches("- ").trim();
                                            if !item_text.is_empty() && !item_text.contains('.') {
                                                findings.push(Finding {
                                                    title: "entity_id without domain prefix".into(),
                                                    description: format!(
                                                        "`{}` is missing a domain prefix (e.g. `sensor.{}`)",
                                                        item_text, item_text
                                                    ),
                                                    severity: Severity::High,
                                                    category: "bug".into(),
                                                    source: Source::LocalAst,
                                                    line_start: item.start_position().row as u32 + 1,
                                                    line_end: item.end_position().row as u32 + 1,
                                                    evidence: vec![],
                                                    calibrator_action: None,
                                                    similar_precedent: vec![],
                                                    canonical_pattern: None,
                                                });
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }
                    }
                    if !is_list && !val_text.is_empty() && !val_text.contains('.') {
                        findings.push(Finding {
                            title: "entity_id without domain prefix".into(),
                            description: format!(
                                "`{}` is missing a domain prefix (e.g. `sensor.{}`)",
                                val_text, val_text
                            ),
                            severity: Severity::High,
                            category: "bug".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }

            // --- Tier 3: service without domain ---
            if key_text == "service" {
                if let Some(value) = node.child_by_field_name("value") {
                    let val_text = source[value.byte_range()].trim();
                    if !val_text.is_empty() && !val_text.contains('.') {
                        findings.push(Finding {
                            title: "service without domain prefix".into(),
                            description: format!(
                                "`{}` is missing a domain prefix (e.g. `light.{}`)",
                                val_text, val_text
                            ),
                            severity: Severity::Medium,
                            category: "bug".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }

            // --- Tier 4: Exposed 0.0.0.0 binding ---
            if key_text == "host" || key_text == "server_host" || key_text == "server" {
                if let Some(value) = node.child_by_field_name("value") {
                    let val_text = source[value.byte_range()].trim();
                    if val_text.contains("0.0.0.0") {
                        findings.push(Finding {
                            title: "Server binding to 0.0.0.0 exposes all interfaces".into(),
                            description: "Binding to 0.0.0.0 makes the server accessible from any network interface. Use 127.0.0.1 for local-only access.".into(),
                            severity: Severity::Medium,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }

            // --- Tier 4: URL with embedded credentials ---
            if let Some(value) = node.child_by_field_name("value") {
                let val_text = source[value.byte_range()].trim();
                if !val_text.starts_with("!secret") && !val_text.starts_with("!include") {
                    if val_text.contains("://") && yaml_url_has_credentials(val_text) {
                        findings.push(Finding {
                            title: "URL contains embedded credentials".into(),
                            description: "URLs with embedded user:password credentials are a security risk. Use environment variables or secret references.".into(),
                            severity: Severity::High,
                            category: "security".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![format!("{}: [REDACTED]", source[key.byte_range()].trim())],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }
        }
    }

    // --- Tier 6: Jinja2 template patterns (on scalar values) ---
    if node.kind() == "plain_scalar" || node.kind() == "double_quote_scalar" || node.kind() == "single_quote_scalar" {
        let val_text = &source[node.byte_range()];

        // Only check values that contain Jinja2 templates
        if val_text.contains("{{") {
            // Pattern 20: states() without availability check
            if val_text.contains("states(") {
                let has_availability = val_text.contains("unavailable") || val_text.contains("unknown");
                if !has_availability {
                    findings.push(Finding {
                        title: "Template uses `states()` without availability check".into(),
                        description: "Templates using states() should check for 'unavailable' and 'unknown' to avoid errors when entities are offline".into(),
                        severity: Severity::Info,
                        category: "quality".into(),
                        source: Source::LocalAst,
                        line_start: line,
                        line_end: end_line,
                        evidence: vec![val_text.chars().take(200).collect()],
                        calibrator_action: None,
                        similar_precedent: vec![],
                        canonical_pattern: None,
                    });
                }
            }

            // Pattern 21: Deprecated dot-notation state access
            let dot_domains = [
                "states.sensor.", "states.binary_sensor.", "states.switch.",
                "states.light.", "states.climate.", "states.cover.",
                "states.fan.", "states.lock.", "states.media_player.",
                "states.automation.", "states.input_boolean.", "states.input_number.",
                "states.input_select.", "states.input_text.", "states.person.",
                "states.device_tracker.", "states.weather.", "states.zone.",
                "states.script.", "states.scene.", "states.group.",
                "states.timer.", "states.counter.", "states.number.",
                "states.select.", "states.button.", "states.vacuum.",
                "states.water_heater.", "states.humidifier.", "states.alarm_control_panel.",
            ];
            if dot_domains.iter().any(|d| val_text.contains(d)) {
                findings.push(Finding {
                    title: "Deprecated dot-notation state access".into(),
                    description: "Use states('sensor.xxx') instead of states.sensor.xxx.state".into(),
                    severity: Severity::Medium,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![val_text.chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
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
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // eval(), document.write(), console.log/debug calls
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
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }

            // document.write XSS
            if func_name == "document.write" {
                findings.push(Finding {
                    title: "Use of `document.write()` is an XSS risk".into(),
                    description: "`document.write()` injects raw HTML into the page. Use DOM APIs or a framework's safe rendering instead.".into(),
                    severity: Severity::Critical,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }

            // console.log / console.debug debug artifacts
            if func_name == "console.log" || func_name == "console.debug" {
                findings.push(Finding {
                    title: format!("`{}` debug artifact left in code", func_name),
                    description: "Debug logging should be removed or replaced with a proper logging framework before production.".into(),
                    severity: Severity::Info,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // Hardcoded secrets in variable declarations
    if node.kind() == "variable_declarator" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let var_name = source[name_node.byte_range()].to_uppercase();
            let secret_names = [
                "SECRET_KEY", "SECRET", "PASSWORD", "PASSWD", "API_KEY",
                "APIKEY", "AUTH_TOKEN", "TOKEN", "PRIVATE_KEY",
            ];
            if secret_names.iter().any(|s| var_name.contains(s)) {
                if let Some(value) = node.child_by_field_name("value") {
                    let val_kind = value.kind();
                    let val_text = &source[value.byte_range()];
                    // Only flag string literals that look like real secrets
                    if val_kind == "string" && val_text.len() > 2 {
                        let inner_len = val_text.len() - 2;
                        let inner = &val_text[1..val_text.len() - 1];
                        let has_upper = inner.chars().any(|c| c.is_ascii_uppercase());
                        let has_digit = inner.chars().any(|c| c.is_ascii_digit());
                        let has_special = inner.chars().any(|c| matches!(c, '-' | '/' | '+' | '='));
                        let looks_like_secret = (has_upper || has_digit || has_special) && inner_len > 8;
                        if looks_like_secret
                            && !val_text.contains("process.env")
                        {
                            findings.push(Finding {
                                title: format!("Hardcoded secret in `{}`", &source[name_node.byte_range()]),
                                description: "Secrets should be loaded from environment variables or a secrets manager, not hardcoded in source.".into(),
                                severity: Severity::High,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![format!("{} = [REDACTED]", &source[name_node.byte_range()])],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // innerHTML / outerHTML XSS
    if node.kind() == "assignment_expression" {
        if let Some(left) = node.child_by_field_name("left") {
            let left_text = &source[left.byte_range()];
            if left_text.ends_with(".innerHTML") || left_text.ends_with(".outerHTML") {
                let prop = if left_text.ends_with(".innerHTML") { "innerHTML" } else { "outerHTML" };
                findings.push(Finding {
                    title: format!("Direct `{}` assignment is an XSS risk", prop),
                    description: format!("Setting `{}` with untrusted data enables XSS. Use `textContent` or a sanitization library.", prop),
                    severity: Severity::High,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // `any` type annotation
    if node.kind() == "type_annotation" {
        let text = &source[node.byte_range()];
        if text.contains(": any") {
            findings.push(Finding {
                title: "Use of `any` type defeats TypeScript's type safety".into(),
                description: "Prefer `unknown`, generics, or a specific type instead of `any`.".into(),
                severity: Severity::Info,
                category: "quality".into(),
                source: Source::LocalAst,
                line_start: line,
                line_end: end_line,
                evidence: vec![source[node.byte_range()].chars().take(100).collect()],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    // Empty catch blocks that silently swallow errors
    if node.kind() == "catch_clause" {
        if let Some(body) = node.child_by_field_name("body") {
            let has_statements = (0..body.named_child_count()).any(|i| {
                body.named_child(i)
                    .map(|c| c.kind() != "comment" && c.kind() != "empty_statement")
                    .unwrap_or(false)
            });
            if !has_statements {
                findings.push(Finding {
                    title: "Empty `catch` block silently swallows errors".into(),
                    description: "An empty catch block hides failures. Log the error, handle it, or rethrow.".into(),
                    severity: Severity::Medium,
                    category: "reliability".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }
    }

    // Sync Node.js APIs inside async functions
    if node.kind() == "call_expression" {
        if let Some(func) = node.child_by_field_name("function") {
            let func_name = &source[func.byte_range()];
            let sync_apis = [
                "readFileSync", "writeFileSync", "mkdirSync", "existsSync",
                "readdirSync", "unlinkSync", "appendFileSync", "copyFileSync",
                "renameSync", "statSync", "accessSync",
            ];
            for api in &sync_apis {
                if func_name.ends_with(api) {
                    if is_in_async_function(node, source) {
                        findings.push(Finding {
                            title: format!("`{}` blocks the event loop in async function", api),
                            description: format!(
                                "Calling synchronous `{}` inside an async function blocks the event loop. Use the async equivalent from `fs/promises`.",
                                api
                            ),
                            severity: Severity::Medium,
                            category: "concurrency".into(),
                            source: Source::LocalAst,
                            line_start: line,
                            line_end: end_line,
                            evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                        break;
                    }
                }
            }
        }
    }

    // Tautological .length >= 0 (always true for arrays/strings)
    if node.kind() == "binary_expression" {
        if let Some(op) = node.child_by_field_name("operator") {
            let op_text = &source[op.byte_range()];
            if op_text == ">=" {
                if let Some(left) = node.child_by_field_name("left") {
                    if let Some(right) = node.child_by_field_name("right") {
                        let right_text = &source[right.byte_range()];
                        let is_length_access = left.kind() == "member_expression"
                            && left.child_by_field_name("property")
                                .map(|p| &source[p.byte_range()] == "length")
                                .unwrap_or(false);
                        if is_length_access && right_text.trim() == "0" {
                            findings.push(Finding {
                                title: "`.length >= 0` is always true".into(),
                                description: "Array and string `.length` is always >= 0. This condition is tautological. Did you mean `.length > 0`?".into(),
                                severity: Severity::Medium,
                                category: "correctness".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Non-null assertion operator (!)
    if node.kind() == "non_null_expression" {
        findings.push(Finding {
            title: "Use of non-null assertion operator `!` bypasses type safety".into(),
            description: "The non-null assertion operator tells TypeScript to ignore possible null/undefined. Use proper null checks instead.".into(),
            severity: Severity::Info,
            category: "quality".into(),
            source: Source::LocalAst,
            line_start: line,
            line_end: end_line,
            evidence: vec![source[node.byte_range()].chars().take(100).collect()],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }
}

fn scan_insecure_bash(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    let kind = node.kind();
    let line = node.start_position().row as u32 + 1;
    let end_line = node.end_position().row as u32 + 1;

    // B11: Missing shebang (root program node only)
    if kind == "program" {
        if !source.starts_with("#!") {
            findings.push(Finding {
                title: "Script has no shebang line".into(),
                description: "Add a shebang (e.g. #!/usr/bin/env bash) so the script runs with the intended interpreter.".into(),
                severity: Severity::Low,
                category: "quality".into(),
                source: Source::LocalAst,
                line_start: 1,
                line_end: 1,
                evidence: vec![],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    // B4: Missing set -e (root program node only)
    if kind == "program" {
        let mut found_set_e = false;
        let limit = node.child_count().min(10);
        for i in 0..limit {
            if let Some(child) = node.child(i) {
                if child.kind() == "command" {
                    let text = &source[child.byte_range()];
                    if text.contains("set") && (text.contains("-e") || text.contains("errexit")) {
                        found_set_e = true;
                        break;
                    }
                }
            }
        }
        if !found_set_e {
            findings.push(Finding {
                title: "Script has no `set -e` -- errors will be silently ignored".into(),
                description: "Add `set -euo pipefail` near the top of the script to fail on errors.".into(),
                severity: Severity::Medium,
                category: "reliability".into(),
                source: Source::LocalAst,
                line_start: 1,
                line_end: 1,
                evidence: vec![],
                calibrator_action: None,
                similar_precedent: vec![],
                canonical_pattern: None,
            });
        }
    }

    // B2: eval usage
    if kind == "command" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = &source[name_node.byte_range()];
            if name == "eval" {
                findings.push(Finding {
                    title: "Use of `eval` is a code injection risk".into(),
                    description: "Avoid `eval` -- use arrays, printf, or parameter expansion instead.".into(),
                    severity: Severity::High,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: line,
                    line_end: end_line,
                    evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }

            // B9: chmod 777
            if name == "chmod" {
                for i in 0..node.child_count() {
                    if let Some(arg) = node.child(i) {
                        let text = &source[arg.byte_range()];
                        if text == "777" {
                            findings.push(Finding {
                                title: "`chmod 777` grants world-writable permissions".into(),
                                description: "Use more restrictive permissions (e.g. 755 or 700).".into(),
                                severity: Severity::Medium,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                            break;
                        }
                    }
                }
            }
        }
    }

    // B3: curl|bash piping
    if kind == "pipeline" {
        let mut saw_curl = false;
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                if child.kind() == "command" {
                    if let Some(name_node) = child.child_by_field_name("name") {
                        let name = &source[name_node.byte_range()];
                        if name == "curl" || name == "wget" {
                            saw_curl = true;
                        } else if saw_curl && (name == "bash" || name == "sh" || name == "zsh") {
                            findings.push(Finding {
                                title: "Piping curl/wget to shell executes untrusted remote code".into(),
                                description: "Download to a file first, inspect it, then execute.".into(),
                                severity: Severity::Critical,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![source[node.byte_range()].chars().take(200).collect()],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                            break;
                        }
                    }
                }
            }
        }
    }

    // B5: Hardcoded secrets
    if kind == "variable_assignment" {
        if let Some(name_node) = node.child_by_field_name("name") {
            let var_name = &source[name_node.byte_range()];
            let upper = var_name.to_uppercase();
            let is_secret_name = upper.contains("PASSWORD")
                || upper.contains("API_KEY")
                || upper.contains("SECRET")
                || upper.contains("TOKEN")
                || upper.contains("APIKEY")
                || upper.contains("PRIVATE_KEY");
            if is_secret_name {
                if let Some(value_node) = node.child_by_field_name("value") {
                    let vkind = value_node.kind();
                    // Skip command substitutions and expansions
                    if vkind != "command_substitution" && vkind != "simple_expansion" && vkind != "expansion" {
                        let val_text = &source[value_node.byte_range()];
                        // Skip if value contains $ (env var reference)
                        if !val_text.contains('$') {
                            findings.push(Finding {
                                title: format!("Hardcoded secret in shell variable `{}`", var_name),
                                description: "Use environment variables or a secrets manager instead of hardcoded values.".into(),
                                severity: Severity::High,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: line,
                                line_end: end_line,
                                evidence: vec![],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                        }
                    }
                }
            }
        }
    }
}

fn scan_insecure_dockerfile(
    node: &tree_sitter::Node,
    source: &str,
    findings: &mut Vec<Finding>,
) {
    let kind = node.kind();

    match kind {
        // D5: ADD instead of COPY for local files
        "add_instruction" => {
            let text = &source[node.byte_range()];
            if !text.contains("http://") && !text.contains("https://") {
                findings.push(Finding {
                    title: "Use COPY instead of ADD for local files".into(),
                    description: "ADD has extra functionality (tar extraction, remote URLs) that can be surprising. Use COPY for simple file copies.".into(),
                    severity: Severity::Medium,
                    category: "quality".into(),
                    source: Source::LocalAst,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    evidence: vec![text.trim().to_string()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }

        // D9: Secrets in ENV/ARG
        "env_instruction" | "arg_instruction" => {
            let text = &source[node.byte_range()];
            let secret_patterns = [
                "PASSWORD", "API_KEY", "SECRET", "TOKEN", "PRIVATE_KEY",
                "ACCESS_KEY", "CREDENTIAL", "AUTH_KEY",
            ];
            // Check if any key name matches a secret pattern and has a hardcoded value
            for line in text.lines() {
                let upper = line.to_uppercase();
                let has_secret_key = secret_patterns.iter().any(|p| upper.contains(p));
                if has_secret_key {
                    // Make sure it has a value that doesn't look like a variable reference
                    if let Some(eq_pos) = line.find('=') {
                        let value = line[eq_pos + 1..].trim().trim_matches('"');
                        if !value.is_empty() && !value.starts_with('$') {
                            findings.push(Finding {
                                title: "Secret hardcoded in Dockerfile ENV/ARG".into(),
                                description: "Secrets should not be hardcoded in Dockerfiles. Use build secrets (--mount=type=secret) or runtime environment variables instead.".into(),
                                severity: Severity::High,
                                category: "security".into(),
                                source: Source::LocalAst,
                                line_start: node.start_position().row as u32 + 1,
                                line_end: node.end_position().row as u32 + 1,
                                evidence: vec![],
                                calibrator_action: None,
                                similar_precedent: vec![],
                                canonical_pattern: None,
                            });
                            break;
                        }
                    }
                }
            }
        }

        // D12: curl|bash in RUN
        "run_instruction" => {
            let text = &source[node.byte_range()];
            let has_downloader = text.contains("curl") || text.contains("wget");
            let has_pipe = text.contains('|');
            let has_shell = text.contains("bash") || text.contains("/sh") || text.contains("| sh");
            if has_downloader && has_pipe && has_shell {
                findings.push(Finding {
                    title: "RUN pipes curl/wget to shell -- executes untrusted remote code".into(),
                    description: "Piping downloaded scripts directly to a shell is dangerous. Download the script first, verify its checksum, then execute.".into(),
                    severity: Severity::Critical,
                    category: "security".into(),
                    source: Source::LocalAst,
                    line_start: node.start_position().row as u32 + 1,
                    line_end: node.end_position().row as u32 + 1,
                    evidence: vec![text.trim().to_string()],
                    calibrator_action: None,
                    similar_precedent: vec![],
                    canonical_pattern: None,
                });
            }
        }

        _ => {}
    }
}

fn analyze_dockerfile_structure(
    tree: &tree_sitter::Tree,
    source: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let root = tree.root_node();

    let mut has_user = false;
    let mut has_healthcheck = false;
    let mut cmd_count = 0u32;
    let mut entrypoint_count = 0u32;

    for i in 0..root.child_count() {
        let Some(child) = root.child(i) else { continue };
        match child.kind() {
            // D1: FROM with latest or no tag
            "from_instruction" => {
                let text = &source[child.byte_range()];
                // Extract the image reference (after FROM, before AS)
                let image_part = text
                    .strip_prefix("FROM")
                    .or_else(|| text.strip_prefix("from"))
                    .unwrap_or(text)
                    .trim();
                // Handle "FROM image AS alias" -- take only the image part
                let image_ref = image_part
                    .split_whitespace()
                    .next()
                    .unwrap_or(image_part);
                if image_ref != "scratch" {
                    let has_tag = image_ref.contains(':');
                    let uses_latest = image_ref.ends_with(":latest");
                    if !has_tag || uses_latest {
                        findings.push(Finding {
                            title: "FROM uses `latest` or untagged image -- builds are not reproducible".into(),
                            description: format!("Pin the image to a specific tag or digest: {}", image_ref),
                            severity: Severity::Medium,
                            category: "reliability".into(),
                            source: Source::LocalAst,
                            line_start: child.start_position().row as u32 + 1,
                            line_end: child.end_position().row as u32 + 1,
                            evidence: vec![image_ref.to_string()],
                            calibrator_action: None,
                            similar_precedent: vec![],
                            canonical_pattern: None,
                        });
                    }
                }
            }

            "user_instruction" => has_user = true,
            "healthcheck_instruction" => has_healthcheck = true,
            "cmd_instruction" => cmd_count += 1,
            "entrypoint_instruction" => entrypoint_count += 1,
            _ => {}
        }
    }

    // D6: No USER instruction
    if !has_user {
        findings.push(Finding {
            title: "No USER instruction -- container runs as root".into(),
            description: "Add a USER instruction to run the container as a non-root user.".into(),
            severity: Severity::Medium,
            category: "security".into(),
            source: Source::LocalAst,
            line_start: 1,
            line_end: 1,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }

    // D8: No HEALTHCHECK
    if !has_healthcheck {
        findings.push(Finding {
            title: "No HEALTHCHECK instruction".into(),
            description: "Add a HEALTHCHECK instruction so the container runtime can detect unhealthy containers.".into(),
            severity: Severity::Low,
            category: "reliability".into(),
            source: Source::LocalAst,
            line_start: 1,
            line_end: 1,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }

    // D11: Multiple CMD/ENTRYPOINT
    if cmd_count > 1 {
        findings.push(Finding {
            title: "Multiple CMD instructions -- only the last one takes effect".into(),
            description: format!("Found {} CMD instructions; only the last one will be used.", cmd_count),
            severity: Severity::Medium,
            category: "bug".into(),
            source: Source::LocalAst,
            line_start: 1,
            line_end: 1,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }
    if entrypoint_count > 1 {
        findings.push(Finding {
            title: "Multiple ENTRYPOINT instructions -- only the last one takes effect".into(),
            description: format!("Found {} ENTRYPOINT instructions; only the last one will be used.", entrypoint_count),
            severity: Severity::Medium,
            category: "bug".into(),
            source: Source::LocalAst,
            line_start: 1,
            line_end: 1,
            evidence: vec![],
            calibrator_action: None,
            similar_precedent: vec![],
            canonical_pattern: None,
        });
    }

    findings
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

    // -- Python-specific patterns (complement ruff, don't duplicate) --

    #[test]
    fn python_hardcoded_secret_assignment() {
        let source = r#"
SECRET_KEY = "hardcoded-secret-12345"
API_KEY = "sk-proj-abcdef"
PASSWORD = "hunter2"
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("Hardcoded secret")),
            "Should flag hardcoded secrets. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_hardcoded_secret_not_flagged_for_empty() {
        let source = r#"
SECRET_KEY = ""
API_KEY = None
PASSWORD = os.environ.get("PASSWORD")
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("Hardcoded secret")),
            "Empty/None/env-loaded values should not be flagged"
        );
    }

    #[test]
    fn python_flask_debug_true() {
        let source = r#"
app = Flask(__name__)
app.run(debug=True, host="0.0.0.0")
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("debug")),
            "Should flag debug=True. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_server_bind_all_interfaces() {
        let source = r#"
app.run(host="0.0.0.0", port=8080)
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("0.0.0.0")),
            "Should flag host=0.0.0.0. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_sql_injection_fstring() {
        let source = r#"
def get_user(username):
    cursor.execute(f"SELECT * FROM users WHERE name='{username}'")
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("SQL") || f.title.contains("sql")),
            "Should flag f-string in SQL execute. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_sql_safe_parameterized() {
        let source = r#"
def get_user(username):
    cursor.execute("SELECT * FROM users WHERE name=%s", (username,))
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("SQL")),
            "Parameterized queries should NOT be flagged"
        );
    }

    #[test]
    fn python_mutable_default_argument() {
        let source = r#"
def process(items=[]):
    items.append(1)
    return items

def also_bad(config={}):
    return config
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("Mutable default")),
            "Should flag mutable default args. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_safe_default_argument() {
        let source = r#"
def process(items=None):
    if items is None:
        items = []
    return items
"#;
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("Mutable default")),
            "None default should NOT be flagged"
        );
    }

    // -- TypeScript patterns --

    #[test]
    fn typescript_hardcoded_secret() {
        let source = "const API_KEY = \"sk-proj-abc123def456\";\nconst SECRET = \"my-secret-key-2024\";";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("Hardcoded secret")),
            "Should flag hardcoded secrets in TS. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn typescript_secret_not_flagged_for_env() {
        let source = "const API_KEY = process.env.API_KEY;\nconst SECRET = \"\";";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(!findings.iter().any(|f| f.title.contains("Hardcoded secret")));
    }

    #[test]
    fn typescript_innerhtml_xss() {
        let source = "element.innerHTML = userInput;";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("innerHTML")));
    }

    #[test]
    fn typescript_document_write_xss() {
        let source = "document.write(data);";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("document.write")));
    }

    #[test]
    fn typescript_console_log_flagged() {
        let source = "function process() { console.log(\"debug\"); return 1; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("console.log")));
    }

    #[test]
    fn typescript_console_error_not_flagged() {
        let source = "function handle() { console.error(\"failed\"); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(!findings.iter().any(|f| f.title.contains("console")));
    }

    #[test]
    fn typescript_any_type_annotation() {
        let source = "function process(data: any) { return data; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("any")));
    }

    #[test]
    fn typescript_non_null_assertion() {
        let source = "const value = getData()!;\nconst name = user!.name;";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(findings.iter().any(|f| f.title.contains("non-null assertion")),
            "Should flag non-null assertions. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    // -- New Python patterns --

    #[test]
    fn python_mutate_while_iterating() {
        let source = "for item in items:\n    if item.bad:\n        items.remove(item)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(findings.iter().any(|f| f.title.contains("Mutating") || f.title.contains("mutating")),
            "Should flag mutating while iterating. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn python_iterate_copy_ok() {
        let source = "for item in list(items):\n    items.remove(item)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(!findings.iter().any(|f| f.title.contains("Mutating") || f.title.contains("mutating")),
            "Iterating over a copy should NOT be flagged");
    }

    #[test]
    fn python_exception_disclosure() {
        let source = "try:\n    process()\nexcept Exception as e:\n    return jsonify({\"error\": str(e)}), 500\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(findings.iter().any(|f| f.title.contains("exception") || f.title.contains("Exception")),
            "Should flag exception disclosure. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn python_exception_logged_ok() {
        let source = "try:\n    process()\nexcept Exception as e:\n    logger.error(str(e))\n    return jsonify({\"error\": \"Internal error\"}), 500\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(!findings.iter().any(|f| f.title.contains("exception") || f.title.contains("Exception")),
            "Logging exception without returning it should NOT be flagged");
    }

    #[test]
    fn python_blocking_result_in_async() {
        let source = "async def process():\n    future = executor.submit(work)\n    return future.result()\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(findings.iter().any(|f| f.title.contains("result()") || f.title.contains("blocking")),
            "Should flag blocking .result() in async. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    // -- YAML patterns --

    #[test]
    fn yaml_duplicate_keys() {
        let source = "automation:\n  alias: First\nautomation:\n  alias: Second\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("Duplicate") || f.title.contains("duplicate")),
            "Should flag duplicate top-level keys. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_hardcoded_secret() {
        let source = "api_key: sk-proj-abc123def456ghi\npassword: SuperSecret123!\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.category == "security"),
            "Should flag hardcoded secrets in YAML. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_secret_tag_not_flagged() {
        let source = "api_key: !secret api_key\npassword: !secret db_password\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.category == "security"),
            "!secret references should NOT be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_include_not_flagged() {
        let source = "api_key: !include secret_file.yaml\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.category == "security"),
            "!include should NOT be flagged"
        );
    }

    #[test]
    fn yaml_automation_missing_id() {
        let source = "automation:\n  - alias: Test Automation\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    actions:\n      - service: light.turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("missing") && f.title.contains("id")),
            "Should flag automation missing id. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_automation_with_id_not_flagged() {
        let source = "automation:\n  - id: auto_001\n    alias: Test\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    actions:\n      - service: light.turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("missing") && f.title.contains("id")),
            "Automation with id should NOT be flagged for missing id"
        );
    }

    #[test]
    fn yaml_automation_missing_mode() {
        let source = "automation:\n  - id: auto_001\n    alias: Test\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    actions:\n      - service: light.turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("mode")),
            "Should flag automation missing mode. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_automation_deprecated_singular() {
        let source = "automation:\n  - id: auto_001\n    alias: Test\n    mode: single\n    trigger:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    action:\n      - service: light.turn_on\n    condition:\n      - condition: state\n        entity_id: binary_sensor.home\n        state: 'on'\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        let deprecated_findings: Vec<_> = findings.iter().filter(|f| f.title.contains("Deprecated") || f.title.contains("deprecated")).collect();
        assert!(
            deprecated_findings.len() >= 3,
            "Should flag trigger, action, and condition as deprecated. Got: {:?}",
            deprecated_findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_automation_plural_forms_not_flagged() {
        let source = "automation:\n  - id: auto_001\n    alias: Test\n    mode: single\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    actions:\n      - service: light.turn_on\n    conditions:\n      - condition: state\n        entity_id: binary_sensor.home\n        state: 'on'\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("Deprecated") || f.title.contains("deprecated")),
            "Plural forms should NOT be flagged as deprecated"
        );
    }

    #[test]
    fn yaml_entity_id_without_domain() {
        let source = "automation:\n  - id: test\n    alias: Test\n    mode: single\n    triggers:\n      - trigger: state\n        entity_id: motion_sensor\n    actions:\n      - service: light.turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("entity_id") && f.title.contains("domain")),
            "Should flag entity_id without domain prefix. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_entity_id_with_domain_ok() {
        let source = "entity_id: binary_sensor.motion\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("entity_id") && f.title.contains("domain")),
            "entity_id with domain should NOT be flagged"
        );
    }

    #[test]
    fn yaml_service_without_domain() {
        let source = "service: turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("service") && f.title.contains("domain")),
            "Should flag service without domain. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_service_with_domain_ok() {
        let source = "service: light.turn_on\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("service") && f.title.contains("domain")),
            "service with domain should NOT be flagged"
        );
    }

    #[test]
    fn yaml_empty_actions() {
        let source = "automation:\n  - id: test\n    alias: Test\n    mode: single\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n    actions:\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("empty") || f.title.contains("Empty")),
            "Should flag empty actions. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_no_false_positives_clean_automation() {
        let source = "automation:\n  - id: auto_001\n    alias: Good Automation\n    mode: restart\n    triggers:\n      - trigger: state\n        entity_id: binary_sensor.motion\n        to: 'on'\n    actions:\n      - service: light.turn_on\n        target:\n          entity_id: light.living_room\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.is_empty(),
            "Clean automation should have no findings. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    // -- Tier 4: URL with credentials --

    #[test]
    fn yaml_url_with_credentials() {
        let source = "database_url: postgres://admin:secret123@db.example.com/mydb\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("credentials") || f.title.contains("Credentials")),
            "Should flag URL with embedded credentials. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_url_without_credentials_ok() {
        let source = "database_url: postgres://db.example.com/mydb\nwebhook: https://hooks.slack.com/services/xxx\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("credentials") || f.title.contains("Credentials")),
            "URL without credentials should NOT be flagged"
        );
    }

    // -- Tier 4: Exposed 0.0.0.0 binding --

    #[test]
    fn yaml_exposed_host_binding() {
        let source = "http:\n  server_host: 0.0.0.0\n  server_port: 8123\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("0.0.0.0")),
            "Should flag 0.0.0.0 binding. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    // -- Tier 5: ESPHome OTA --

    #[test]
    fn yaml_esphome_ota_no_password() {
        let source = "esphome:\n  name: test-device\n\nota:\n  platform: esphome\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("OTA") || f.title.contains("ota")),
            "Should flag ESPHome OTA without password. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_esphome_ota_with_password_ok() {
        let source = "esphome:\n  name: test-device\n\nota:\n  platform: esphome\n  password: !secret ota_password\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("OTA") || f.title.contains("ota")),
            "ESPHome OTA with password should NOT be flagged"
        );
    }

    // -- Tier 5: ESPHome API --

    #[test]
    fn yaml_esphome_api_no_encryption() {
        let source = "esphome:\n  name: test-device\n\napi:\n  services: []\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("API") || f.title.contains("encryption")),
            "Should flag ESPHome API without encryption. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_esphome_api_with_encryption_ok() {
        let source = "esphome:\n  name: test-device\n\napi:\n  encryption:\n    key: !secret api_encryption_key\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("API") || f.title.contains("encryption")),
            "ESPHome API with encryption should NOT be flagged"
        );
    }

    // -- Tier 6: Jinja2 template patterns --

    #[test]
    fn yaml_jinja2_states_without_availability() {
        let source = "value_template: \"{{ states('sensor.temperature') | float }}\"\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("states()") || f.title.contains("availability")),
            "Should flag states() without availability check. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_jinja2_states_with_availability_ok() {
        let source = "value_template: \"{{ states('sensor.temperature') if states('sensor.temperature') not in ['unavailable', 'unknown'] else 0 }}\"\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("states()") && f.title.contains("availability")),
            "states() with availability check should NOT be flagged"
        );
    }

    #[test]
    fn yaml_jinja2_deprecated_dot_notation() {
        let source = "value_template: \"{{ states.sensor.temperature.state }}\"\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            findings.iter().any(|f| f.title.contains("dot-notation") || f.title.contains("Deprecated")),
            "Should flag deprecated dot-notation. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn yaml_jinja2_proper_states_call_ok() {
        let source = "value_template: \"{{ states('sensor.temperature') }}\"\n";
        let tree = parse(source, Language::Yaml).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Yaml);
        assert!(
            !findings.iter().any(|f| f.title.contains("dot-notation")),
            "Proper states() call should NOT be flagged for dot-notation"
        );
    }

    // -- Bash patterns --

    #[test]
    fn bash_eval_usage() {
        let source = "#!/bin/bash\neval \"$user_input\"\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.title.contains("eval")),
            "Should flag eval usage. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_curl_pipe_bash() {
        let source = "#!/bin/bash\ncurl -sL https://example.com/install.sh | bash\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.severity == Severity::Critical),
            "Should flag curl|bash as critical. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_wget_pipe_sh() {
        let source = "#!/bin/bash\nwget -qO- https://example.com/setup | sh\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.severity == Severity::Critical),
            "Should flag wget|sh as critical. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_missing_set_e() {
        let source = "#!/bin/bash\necho hello\nrm -rf /tmp/stuff\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.title.contains("set -e")),
            "Should flag missing set -e. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_set_euo_pipefail_ok() {
        let source = "#!/bin/bash\nset -euo pipefail\necho hello\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(!findings.iter().any(|f| f.title.contains("set -e")),
            "Script with set -e should NOT be flagged");
    }

    #[test]
    fn bash_hardcoded_secret() {
        let source = "#!/bin/bash\nset -e\nAPI_KEY=\"sk-proj-abc123def456\"\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.category == "security" && f.title.contains("secret")),
            "Should flag hardcoded secrets. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_secret_from_command_ok() {
        let source = "#!/bin/bash\nset -e\nAPI_KEY=$(vault get api-key)\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(!findings.iter().any(|f| f.title.contains("secret")),
            "Secrets from command substitution should NOT be flagged");
    }

    #[test]
    fn bash_chmod_777() {
        let source = "#!/bin/bash\nset -e\nchmod 777 /var/www/app\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.title.contains("chmod") && f.title.contains("777")),
            "Should flag chmod 777. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_missing_shebang() {
        let source = "echo hello\nrm -rf /tmp/stuff\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(findings.iter().any(|f| f.title.contains("shebang")),
            "Should flag missing shebang. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn bash_shebang_present_ok() {
        let source = "#!/usr/bin/env bash\nset -e\necho hello\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        assert!(!findings.iter().any(|f| f.title.contains("shebang")),
            "Script with shebang should NOT be flagged");
    }

    #[test]
    fn bash_clean_script_no_serious_findings() {
        let source = "#!/usr/bin/env bash\nset -euo pipefail\n\nmain() {\n  echo \"deploying\"\n}\n\nmain \"$@\"\n";
        let tree = parse(source, Language::Bash).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Bash);
        let serious = findings.iter().filter(|f| f.severity >= Severity::Medium).count();
        assert_eq!(serious, 0, "Clean script should have no serious findings. Got: {:?}",
            findings.iter().map(|f| (&f.severity, &f.title)).collect::<Vec<_>>());
    }

    // -- Dockerfile patterns --

    #[test]
    fn dockerfile_from_latest() {
        let source = "FROM node:latest\nRUN npm install\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("latest")),
            "Should flag FROM :latest. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_from_no_tag() {
        let source = "FROM node\nRUN npm install\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("latest") || f.title.contains("untagged")),
            "Should flag untagged FROM. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_from_pinned_ok() {
        let source = "FROM node:18-alpine\nRUN npm install\nUSER node\nHEALTHCHECK CMD curl -f http://localhost:3000/ || exit 1\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(!findings.iter().any(|f| f.title.contains("latest") || f.title.contains("untagged")),
            "Pinned image should NOT be flagged");
    }

    #[test]
    fn dockerfile_no_user() {
        let source = "FROM node:18\nRUN npm install\nCOPY . /app\nCMD [\"node\", \"app.js\"]\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("USER") || f.title.contains("root")),
            "Should flag missing USER. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_has_user_ok() {
        let source = "FROM node:18\nRUN npm install\nUSER node\nHEALTHCHECK CMD curl -f http://localhost/ || exit 1\nCMD [\"node\", \"app.js\"]\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(!findings.iter().any(|f| f.title.contains("USER") && f.title.contains("missing")),
            "Dockerfile with USER should NOT be flagged");
    }

    #[test]
    fn dockerfile_add_instead_of_copy() {
        let source = "FROM node:18\nADD . /app\nUSER node\nHEALTHCHECK CMD true\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("ADD") || f.title.contains("COPY")),
            "Should flag ADD for local files. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_add_url_ok() {
        let source = "FROM node:18\nADD https://example.com/file.tar.gz /tmp/\nUSER node\nHEALTHCHECK CMD true\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(!findings.iter().any(|f| f.title.contains("ADD") && f.title.contains("COPY")),
            "ADD with URL should NOT suggest COPY");
    }

    #[test]
    fn dockerfile_no_healthcheck() {
        let source = "FROM node:18\nRUN npm install\nUSER node\nCMD [\"node\", \"app.js\"]\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("HEALTHCHECK")),
            "Should flag missing HEALTHCHECK. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_secrets_in_env() {
        let source = "FROM node:18\nENV API_KEY=sk-proj-abc123def456\nUSER node\nHEALTHCHECK CMD true\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.category == "security" && (f.title.contains("secret") || f.title.contains("Secret"))),
            "Should flag secrets in ENV. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_curl_pipe_bash_in_run() {
        let source = "FROM ubuntu:22.04\nRUN curl -sL https://deb.nodesource.com/setup | bash -\nUSER node\nHEALTHCHECK CMD true\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.severity == Severity::Critical),
            "Should flag curl|bash in RUN. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    #[test]
    fn dockerfile_multiple_cmd() {
        let source = "FROM node:18\nCMD [\"echo\", \"first\"]\nCMD [\"echo\", \"second\"]\nUSER node\nHEALTHCHECK CMD true\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        assert!(findings.iter().any(|f| f.title.contains("Multiple CMD") || f.title.contains("multiple")),
            "Should flag multiple CMD. Got: {:?}", findings.iter().map(|f| &f.title).collect::<Vec<_>>());
    }

    // -- Empty catch block detection (TypeScript) --

    #[test]
    fn ts_bare_catch_empty_body() {
        let source = "try { doStuff(); } catch (e) { }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("Empty `catch` block")),
            "Should flag empty catch. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_bare_catch_comment_only() {
        let source = "try { doStuff(); } catch (e) { /* ignore */ }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("Empty `catch` block")),
            "Catch with only comments should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_catch_with_logging_not_flagged() {
        let source = "try { doStuff(); } catch (e) { console.error(e); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            !findings.iter().any(|f| f.title.contains("Empty `catch` block")),
            "Catch with real statements should not be flagged"
        );
    }

    #[test]
    fn ts_catch_with_rethrow_not_flagged() {
        let source = "try { doStuff(); } catch (e) { throw e; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            !findings.iter().any(|f| f.title.contains("Empty `catch` block")),
            "Catch that rethrows should not be flagged"
        );
    }

    #[test]
    fn ts_empty_catch_with_semicolon_flagged() {
        let source = "try { doStuff(); } catch (e) { ; }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("Empty `catch` block")),
            "Catch with only empty statement should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    // -- Sync APIs in async functions (TypeScript) --

    #[test]
    fn ts_sync_api_in_async_function() {
        let source = "async function loadConfig() { const data = fs.readFileSync('config.json'); return JSON.parse(data); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("readFileSync")),
            "Should flag sync API in async function. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_sync_api_in_async_arrow() {
        let source = "const save = async () => { fs.writeFileSync('out.json', data); };";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("writeFileSync")),
            "Should flag sync API in async arrow. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_sync_api_in_sync_function_not_flagged() {
        let source = "function loadConfig() { const data = fs.readFileSync('config.json'); return JSON.parse(data); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            !findings.iter().any(|f| f.title.contains("readFileSync")),
            "Sync API in sync function should not be flagged"
        );
    }

    #[test]
    fn ts_async_method_with_sync_api() {
        let source = "class Loader { async load() { return fs.existsSync('file'); } }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("existsSync")),
            "Should flag sync API in async method. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_sync_api_in_exported_async_function() {
        let source = "export async function loadConfig() { const data = fs.readFileSync('config.json'); return JSON.parse(data); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains("readFileSync")),
            "Should flag sync API in exported async function. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_sync_api_not_flagged_for_async_variable_name() {
        let source = "const asyncHandler = () => { fs.readFileSync('config.json'); };";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            !findings.iter().any(|f| f.title.contains("readFileSync")),
            "Variable named asyncHandler should not trigger async detection"
        );
    }

    // -- open() without encoding (Python) --

    #[test]
    fn python_open_without_encoding() {
        let source = "with open('config.yaml') as f:\n    data = f.read()\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("encoding")),
            "open() without encoding should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_open_with_encoding_not_flagged() {
        let source = "with open('config.yaml', encoding='utf-8') as f:\n    data = f.read()\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("encoding")),
            "open() with encoding should not be flagged"
        );
    }

    #[test]
    fn python_open_binary_mode_not_flagged() {
        let source = "with open('image.png', 'rb') as f:\n    data = f.read()\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("encoding")),
            "Binary mode open should not be flagged"
        );
    }

    #[test]
    fn python_open_write_without_encoding() {
        let source = "f = open('output.txt', 'w')\nf.write(data)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("encoding")),
            "open('w') without encoding should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_open_xb_mode_not_flagged() {
        let source = "with open('data.bin', 'xb') as f:\n    f.write(data)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("encoding")),
            "xb binary mode should not be flagged"
        );
    }

    #[test]
    fn python_open_rplusb_mode_not_flagged() {
        let source = "with open('data.bin', 'r+b') as f:\n    data = f.read()\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("encoding")),
            "r+b binary mode should not be flagged"
        );
    }

    // -- Bare except: pass (Python) --

    #[test]
    fn python_bare_except_pass() {
        let source = "try:\n    do_stuff()\nexcept:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("swallows")),
            "Bare except with pass should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_except_exception_pass() {
        let source = "try:\n    do_stuff()\nexcept Exception:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("swallows")),
            "except Exception with pass should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_except_with_logging_not_flagged() {
        let source = "try:\n    do_stuff()\nexcept Exception as e:\n    logger.error(e)\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("pass")),
            "Except with logging should not be flagged"
        );
    }

    #[test]
    fn python_specific_except_pass_not_flagged() {
        let source = "try:\n    do_stuff()\nexcept FileNotFoundError:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            !findings.iter().any(|f| f.title.contains("Bare")),
            "Specific exception with pass is intentional, should not be flagged"
        );
    }

    #[test]
    fn python_except_base_exception_pass() {
        let source = "try:\n    do_stuff()\nexcept BaseException:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("swallows")),
            "except BaseException with pass should be flagged. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn python_except_exception_as_e_pass_flagged() {
        let source = "try:\n    do_stuff()\nexcept Exception as e:\n    pass\n";
        let tree = parse(source, Language::Python).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Python);
        assert!(
            findings.iter().any(|f| f.title.contains("swallows")),
            "except Exception as e: pass still swallows errors. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    // -- Tautological .length >= 0 (TypeScript) --

    #[test]
    fn ts_tautological_length_gte_zero() {
        let source = "if (items.length >= 0) { doStuff(); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains(".length >= 0")),
            "Should flag tautological length check. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn ts_valid_length_gt_zero_not_flagged() {
        let source = "if (items.length > 0) { doStuff(); }";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            !findings.iter().any(|f| f.title.contains("tautological") || f.title.contains(".length")),
            "length > 0 is valid and should not be flagged"
        );
    }

    #[test]
    fn ts_tautological_length_in_condition() {
        let source = "const valid = results.length >= 0 && isReady;";
        let tree = parse(source, Language::TypeScript).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::TypeScript);
        assert!(
            findings.iter().any(|f| f.title.contains(".length >= 0")),
            "Should flag tautological length in any expression. Got: {:?}",
            findings.iter().map(|f| &f.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn dockerfile_clean_no_serious_findings() {
        let source = "FROM node:18-alpine AS build\nWORKDIR /app\nCOPY package*.json ./\nRUN npm ci\nCOPY . .\nFROM node:18-alpine\nWORKDIR /app\nCOPY --from=build /app .\nUSER node\nHEALTHCHECK CMD curl -f http://localhost:3000/ || exit 1\nCMD [\"node\", \"server.js\"]\n";
        let tree = parse(source, Language::Dockerfile).unwrap();
        let findings = analyze_insecure_patterns(&tree, source, Language::Dockerfile);
        let serious = findings.iter().filter(|f| f.severity >= Severity::Medium).count();
        assert_eq!(serious, 0, "Clean Dockerfile should have no serious findings. Got: {:?}",
            findings.iter().filter(|f| f.severity >= Severity::Medium).map(|f| (&f.severity, &f.title)).collect::<Vec<_>>());
    }

}
