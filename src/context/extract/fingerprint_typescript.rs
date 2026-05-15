//! TypeScript AST structural fingerprinter.
//!
//! Walks a tree-sitter parse tree (via ast-grep) to extract structural features
//! from TypeScript functions and methods, producing a [`StructuralFingerprint`]
//! that can be projected into a fixed-size vector for similarity search.

use ast_grep_core::Doc;
use ast_grep_language::{LanguageExt, SupportLang};

use super::fingerprint::{
    ControlFlowSketch, SemanticCounts, SignatureShape, StructuralFingerprint, TypeCategory,
    MIN_BODY_NODE_COUNT,
};

/// Node kinds that represent function-like constructs in tree-sitter-typescript.
const FUNCTION_KINDS: &[&str] = &[
    "function_declaration",
    "method_definition",
    "arrow_function",
    "function",
];

/// Stateless fingerprinter for TypeScript source code.
pub struct TypeScriptFingerprinter;

impl TypeScriptFingerprinter {
    /// Convenience entry point for testing: parse `src`, find the first
    /// function-like node, and fingerprint it.
    pub fn fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint> {
        let root = SupportLang::TypeScript.ast_grep(src);
        let root_node = root.root();
        let func_node = find_first_function(&root_node)?;
        self.fingerprint_node(&func_node, src)
    }

    /// Extract a [`StructuralFingerprint`] from a function/method AST node.
    ///
    /// Accepts `function_declaration`, `method_definition`, `arrow_function`,
    /// and `function` node kinds.
    ///
    /// Returns `None` if the function body has fewer than [`MIN_BODY_NODE_COUNT`]
    /// descendant nodes (trivial function filter).
    pub fn fingerprint_all_functions(&self, src: &str) -> Vec<(String, StructuralFingerprint)> {
        let root = SupportLang::TypeScript.ast_grep(src);
        let root_node = root.root();
        let func_nodes: Vec<_> = root_node
            .dfs()
            .filter(|n| FUNCTION_KINDS.contains(&n.kind().as_ref()))
            .collect();
        let mut results = Vec::new();
        for node in &func_nodes {
            let name = node
                .children()
                .find(|c| {
                    let k = c.kind();
                    k.as_ref() == "identifier" || k.as_ref() == "property_identifier"
                })
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            if let Some(fp) = self.fingerprint_node(node, src) {
                results.push((name, fp));
            }
        }
        results
    }

    pub fn fingerprint_node<'a, D: Doc>(
        &self,
        node: &'a ast_grep_core::Node<'a, D>,
        source: &str,
    ) -> Option<StructuralFingerprint> {
        let kind = node.kind();
        let kind_str = kind.as_ref();
        if !FUNCTION_KINDS.contains(&kind_str) {
            return None;
        }

        // Find the function body. In tree-sitter-typescript, function bodies
        // are `statement_block` nodes. Arrow functions may also have an
        // expression body (parenthesized_expression, etc.), but we only
        // fingerprint those with a statement_block (non-trivial bodies).
        let body = node
            .children()
            .find(|c| c.kind().as_ref() == "statement_block")?;

        // Trivial function filter: count all descendant nodes in the body.
        let body_node_count = body.dfs().count();
        if body_node_count < MIN_BODY_NODE_COUNT {
            return None;
        }

        let signature = extract_signature(node, source);
        let control_flow = extract_control_flow(&body);
        let semantic_counts = extract_semantic_counts(&body);

        Some(StructuralFingerprint {
            signature,
            control_flow,
            semantic_counts,
        })
    }
}

/// Find the first function-like node in a DFS walk of the tree.
fn find_first_function<'a, D: Doc>(
    root: &'a ast_grep_core::Node<'a, D>,
) -> Option<ast_grep_core::Node<'a, D>> {
    root.dfs()
        .find(|n| FUNCTION_KINDS.contains(&n.kind().as_ref()))
}

/// Extract signature shape from a function/method node.
fn extract_signature<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    source: &str,
) -> SignatureShape {
    let mut shape = SignatureShape::default();

    let kind = node.kind();
    let kind_str = kind.as_ref();

    // Detect if this is a method inside a class.
    let in_class = is_inside_class(node);

    // Find the formal_parameters node.
    if let Some(params) = node
        .children()
        .find(|c| c.kind().as_ref() == "formal_parameters")
    {
        extract_params(&params, &mut shape, source);
    }

    // Extract return type annotation.
    extract_return_type(node, &mut shape, source);

    // Determine method/static/constructor status.
    if kind_str == "method_definition" && in_class {
        // Check for constructor.
        let is_constructor = node
            .children()
            .find(|c| c.kind().as_ref() == "property_identifier")
            .map(|c| c.text().as_ref() == "constructor")
            .unwrap_or(false);

        // Check for static modifier.
        let is_static = has_static_modifier(node);

        if is_constructor {
            shape.is_constructor = true;
            shape.is_method = true;
            shape.has_self = true;
        } else if is_static {
            shape.is_static = true;
            shape.is_method = true;
        } else {
            // Regular instance method — has implicit `this`.
            shape.is_method = true;
            shape.has_self = true;
        }
    } else if in_class {
        // Arrow function or function expression assigned as a class property.
        shape.is_method = true;
        shape.has_self = true;
    }

    shape
}

/// Check if a node is inside a class body.
fn is_inside_class<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    node.ancestors().any(|a| {
        let ak = a.kind();
        let kind = ak.as_ref();
        kind == "class_declaration" || kind == "class"
    })
}

/// Check if a method_definition has a `static` modifier.
///
/// In tree-sitter-typescript, `static` appears as a direct child text token
/// before the method name.
fn has_static_modifier<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> bool {
    // The static keyword appears as a child node with kind "static" or
    // the method text starts with "static".
    for child in node.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        // In some tree-sitter versions it is a bare keyword node.
        if kind == "static" {
            return true;
        }
        // Stop scanning once we hit the method name or params.
        if kind == "property_identifier" || kind == "formal_parameters" {
            break;
        }
    }
    false
}

/// Extract parameters from a `formal_parameters` node.
///
/// Handles TypeScript's explicit `this: Type` first parameter by setting
/// `has_self` and excluding it from arity.
fn extract_params<D: Doc>(
    params: &ast_grep_core::Node<'_, D>,
    shape: &mut SignatureShape,
    source: &str,
) {
    let mut is_first = true;
    for child in params.children() {
        let ck = child.kind();
        let child_kind = ck.as_ref();
        match child_kind {
            // Regular parameter: `name` or `name: type`
            "required_parameter" | "optional_parameter" => {
                // tree-sitter-typescript uses kind "this" (not "identifier")
                // for the `this` keyword inside a parameter node.
                let has_this_child = child.children().any(|c| c.kind().as_ref() == "this");
                if is_first && has_this_child {
                    shape.has_self = true;
                    is_first = false;
                    continue;
                }
                is_first = false;
                shape.arity += 1;
                let cat = classify_param_type(&child, source);
                shape.param_categories.push(cat);
            }
            // Rest parameter: `...args: type[]`
            "rest_parameter" => {
                is_first = false;
                shape.arity += 1;
                shape.param_categories.push(TypeCategory::Col);
            }
            _ => {}
        }
    }
}

/// Classify a parameter's type by finding the type annotation.
fn classify_param_type<D: Doc>(
    param: &ast_grep_core::Node<'_, D>,
    _source: &str,
) -> TypeCategory {
    // In tree-sitter-typescript, a parameter with a type annotation has a
    // `type_annotation` child that wraps the actual type node.
    if let Some(type_ann) = param
        .children()
        .find(|c| c.kind().as_ref() == "type_annotation")
    {
        return classify_type_annotation(&type_ann);
    }
    TypeCategory::Unknown
}

/// Classify the type inside a `type_annotation` node (`: type`).
fn classify_type_annotation<D: Doc>(ann: &ast_grep_core::Node<'_, D>) -> TypeCategory {
    // The type_annotation node contains a colon and then the type node.
    for child in ann.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == ":" {
            continue;
        }
        return classify_type_node(&child);
    }
    TypeCategory::Unknown
}

/// Classify a type expression node using the TypeScript type classifier.
fn classify_type_node<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> TypeCategory {
    let kind = node.kind();
    let kind_str = kind.as_ref();
    match kind_str {
        "predefined_type" | "type_identifier" => {
            TypeCategory::classify_typescript(node.text().as_ref())
        }
        "generic_type" => {
            // e.g. `Array<string>`, `Map<K, V>`, `Promise<T>`
            // Classify the outermost type identifier.
            if let Some(name) = node
                .children()
                .find(|c| c.kind().as_ref() == "type_identifier")
            {
                return TypeCategory::classify_typescript(name.text().as_ref());
            }
            TypeCategory::Generic
        }
        "array_type" => TypeCategory::Col,
        "union_type" => {
            // Check if it looks like an optional (contains undefined/null).
            let has_null_like = node.children().any(|c| {
                let text = c.text();
                let t = text.as_ref();
                t == "undefined" || t == "null"
            });
            if has_null_like {
                TypeCategory::Opt
            } else {
                TypeCategory::Generic
            }
        }
        "function_type" | "constructor_type" => TypeCategory::Fn,
        "parenthesized_type" => {
            // Unwrap and classify inner type.
            for child in node.children() {
                let ck = child.kind();
                if ck.as_ref() != "(" && ck.as_ref() != ")" {
                    return classify_type_node(&child);
                }
            }
            TypeCategory::Unknown
        }
        "literal_type" => TypeCategory::Prim,
        "tuple_type" => TypeCategory::Col,
        "object_type" | "intersection_type" => TypeCategory::Generic,
        _ => TypeCategory::Unknown,
    }
}

/// Extract the return type annotation from a function node.
///
/// In tree-sitter-typescript, the return type is a `type_annotation` child
/// that appears after the formal_parameters.
fn extract_return_type<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    shape: &mut SignatureShape,
    _source: &str,
) {
    // Look for a type_annotation child that comes after formal_parameters.
    // In tree-sitter-typescript, function return types are represented as
    // a type_annotation child of the function node (not inside params).
    let mut saw_params = false;
    for child in node.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == "formal_parameters" {
            saw_params = true;
            continue;
        }
        if saw_params && kind == "type_annotation" {
            let cat = classify_type_annotation(&child);
            shape.return_category = Some(cat);

            // Check for wrapping (Promise<T> wraps like Result).
            let nesting = count_type_nesting(&child);
            shape.return_nesting = nesting;

            // Detect Promise wrapping (analogous to Result in Rust).
            let wraps_promise = child.dfs().any(|d| {
                let dk = d.kind();
                dk.as_ref() == "type_identifier" && d.text().as_ref() == "Promise"
            });
            shape.return_wraps_result = wraps_promise;

            // Detect optional return (union with undefined/null).
            let wraps_optional = child.dfs().any(|d| {
                let dk = d.kind();
                let dks = dk.as_ref();
                if dks == "predefined_type" || dks == "type_identifier" {
                    let t = d.text();
                    let ts = t.as_ref();
                    ts == "undefined" || ts == "null" || ts == "void"
                } else {
                    false
                }
            });
            shape.return_wraps_option = wraps_optional;
            return;
        }
        // Stop if we hit the body.
        if kind == "statement_block" {
            break;
        }
    }
}

/// Count the nesting depth of generic type arguments within a type annotation.
fn count_type_nesting<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> u8 {
    let mut max_depth: u8 = 0;
    for child in node.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == "type_arguments" {
            let inner_max = child
                .children()
                .filter(|c| c.kind().as_ref() == "generic_type")
                .map(|c| 1 + count_type_nesting(&c))
                .max()
                .unwrap_or(1);
            max_depth = max_depth.max(inner_max);
        } else if kind == "generic_type" {
            let inner = count_type_nesting(&child);
            max_depth = max_depth.max(inner);
        }
    }
    max_depth
}

/// Extract control-flow features from a function body node.
fn extract_control_flow<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> ControlFlowSketch {
    let mut cf = ControlFlowSketch::default();

    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "if_statement" => cf.branches += 1,
            "else_clause" => cf.branches += 1,
            "for_statement" | "for_in_statement" | "while_statement" | "do_statement" => {
                cf.loops += 1;
            }
            "return_statement" | "throw_statement" => cf.early_returns += 1,
            "try_statement" => cf.error_propagations += 1,
            "catch_clause" => cf.error_propagations += 1,
            "await_expression" => cf.awaits += 1,
            "arrow_function" => cf.closures += 1,
            "switch_case" => cf.match_arms += 1,
            _ => {}
        }
    }

    cf
}

/// Extract semantic counts from a function body node.
fn extract_semantic_counts<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> SemanticCounts {
    let mut sc = SemanticCounts::default();

    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "call_expression" => sc.calls += 1,
            "assignment_expression" | "augmented_assignment_expression" => sc.assignments += 1,
            "variable_declarator" => sc.assignments += 1,
            "member_expression" => sc.member_access += 1,
            "subscript_expression" => sc.index_ops += 1,
            "binary_expression" | "ternary_expression" => sc.binary_ops += 1,
            "array" => sc.collection_literals += 1,
            "object" => sc.collection_literals += 1,
            "type_annotation" => sc.type_annotations += 1,
            "arrow_function" => sc.lambdas += 1,
            _ => {}
        }
    }

    sc
}
