//! Rust AST structural fingerprinter.
//!
//! Walks a tree-sitter parse tree (via ast-grep) to extract structural features
//! from Rust functions and methods, producing a [`StructuralFingerprint`] that
//! can be projected into a fixed-size vector for similarity search.

use ast_grep_core::Doc;
use ast_grep_language::{LanguageExt, SupportLang};

use super::fingerprint::{
    ControlFlowSketch, MIN_BODY_NODE_COUNT, SemanticCounts, SignatureShape, StructuralFingerprint,
    TypeCategory,
};

/// Stateless fingerprinter for Rust source code.
pub struct RustFingerprinter;

impl RustFingerprinter {
    /// Convenience entry point for testing: parse `src`, find the first
    /// `function_item` node, and fingerprint it.
    pub fn fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint> {
        let root = SupportLang::Rust.ast_grep(src);
        let root_node = root.root();
        let func_node = find_first_function(&root_node)?;
        self.fingerprint_node(&func_node, src)
    }

    /// Extract a [`StructuralFingerprint`] from a function/method AST node.
    ///
    /// Returns `None` if the function body has fewer than [`MIN_BODY_NODE_COUNT`]
    /// descendant nodes (trivial function filter).
    /// Fingerprint all non-trivial functions in a source file.
    /// Returns `(function_name, fingerprint)` pairs.
    pub fn fingerprint_all_functions(&self, src: &str) -> Vec<(String, StructuralFingerprint)> {
        let root = SupportLang::Rust.ast_grep(src);
        let root_node = root.root();
        let func_nodes: Vec<_> = root_node
            .dfs()
            .filter(|n| {
                let k = n.kind();
                k.as_ref() == "function_item" || k.as_ref() == "function_signature_item"
            })
            .collect();
        let mut results = Vec::new();
        for node in &func_nodes {
            let name = node
                .children()
                .find(|c| c.kind().as_ref() == "identifier")
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
        if kind_str != "function_item" && kind_str != "function_signature_item" {
            return None;
        }

        // Find the function body block.
        let body = node.children().find(|c| c.kind().as_ref() == "block")?;

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

/// Find the first `function_item` in a DFS walk of the tree.
fn find_first_function<'a, D: Doc>(
    root: &'a ast_grep_core::Node<'a, D>,
) -> Option<ast_grep_core::Node<'a, D>> {
    root.dfs().find(|n| n.kind().as_ref() == "function_item")
}

/// Extract signature shape from a function/method node.
fn extract_signature<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    source: &str,
) -> SignatureShape {
    let mut shape = SignatureShape::default();

    // Detect if inside an impl block by walking ancestors.
    let in_impl = node.ancestors().any(|a| a.kind().as_ref() == "impl_item");

    // Find the parameters node.
    if let Some(params) = node.children().find(|c| c.kind().as_ref() == "parameters") {
        extract_params(&params, &mut shape, source);
    }

    // Extract return type.
    extract_return_type(node, &mut shape, source);

    shape.is_method = in_impl;
    shape.is_static = in_impl && !shape.has_self;

    // Constructor heuristic: inside impl, no self, returns Self.
    if shape.is_static {
        if let Some(ref ret) = shape.return_category
            && *ret == TypeCategory::SelfRef
        {
            shape.is_constructor = true;
        }
        if let Some(name_node) = node.children().find(|c| c.kind().as_ref() == "identifier")
            && name_node.text().as_ref() == "new"
        {
            shape.is_constructor = true;
        }
    }

    shape
}

/// Extract parameters from a `parameters` node, populating `shape`.
fn extract_params<D: Doc>(
    params: &ast_grep_core::Node<'_, D>,
    shape: &mut SignatureShape,
    source: &str,
) {
    for child in params.children() {
        let ck = child.kind();
        let child_kind = ck.as_ref();
        match child_kind {
            "self_parameter" => {
                shape.has_self = true;
                let text = child.text();
                let text_str = text.as_ref();
                shape.is_mut_self = text_str.contains("&mut");
            }
            "parameter" => {
                shape.arity += 1;
                // Try to classify the type of this parameter.
                let cat = classify_param_type(&child, source);
                shape.param_categories.push(cat);
            }
            _ => {}
        }
    }
}

/// Classify a parameter's type by finding the type annotation within a
/// `parameter` node. Extracts the outermost type name.
fn classify_param_type<D: Doc>(param: &ast_grep_core::Node<'_, D>, _source: &str) -> TypeCategory {
    // A parameter node typically has: pattern, ":", type
    // We look for a type child (could be type_identifier, generic_type,
    // reference_type, etc.)
    for child in param.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        match kind {
            "type_identifier" => {
                return TypeCategory::classify_rust(child.text().as_ref());
            }
            "generic_type" => {
                // e.g. Vec<String> - the outermost type is the first type_identifier child
                if let Some(ti) = child
                    .children()
                    .find(|c| c.kind().as_ref() == "type_identifier")
                {
                    return TypeCategory::classify_rust(ti.text().as_ref());
                }
            }
            "reference_type" => {
                return TypeCategory::Ref;
            }
            "function_type" => {
                return TypeCategory::Fn;
            }
            "primitive_type" => {
                return TypeCategory::classify_rust(child.text().as_ref());
            }
            "scoped_type_identifier" => {
                // e.g. std::io::Error - classify the last segment
                let text = child.text();
                let text_str = text.as_ref();
                if let Some(last) = text_str.rsplit("::").next() {
                    return TypeCategory::classify_rust(last);
                }
            }
            _ => {}
        }
    }
    TypeCategory::Unknown
}

/// Extract the return type from a function node. Looks for a `->` followed by
/// a type node in the function's children.
fn extract_return_type<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    shape: &mut SignatureShape,
    _source: &str,
) {
    // In tree-sitter-rust, the return type is behind the field name "return_type"
    // on a function_item. Try field access first, then fall back to scanning children.
    let ret_type = node
        .field("return_type")
        .or_else(|| find_return_type_child(node));

    let Some(ret) = ret_type else {
        return;
    };

    let (cat, nesting, wraps_option, wraps_result) = classify_return_type(&ret);
    shape.return_category = Some(cat);
    shape.return_nesting = nesting;
    shape.return_wraps_option = wraps_option;
    shape.return_wraps_result = wraps_result;
}

/// Fallback: scan children for a type node that appears after `->`.
fn find_return_type_child<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
) -> Option<ast_grep_core::Node<'a, D>> {
    let mut saw_arrow = false;
    for child in node.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == "->" {
            saw_arrow = true;
            continue;
        }
        if saw_arrow && is_type_node(kind) {
            return Some(child);
        }
    }
    None
}

/// Check if a node kind represents a type in tree-sitter-rust.
fn is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "type_identifier"
            | "generic_type"
            | "reference_type"
            | "primitive_type"
            | "scoped_type_identifier"
            | "function_type"
            | "tuple_type"
            | "unit_type"
            | "array_type"
            | "pointer_type"
            | "bounded_type"
            | "dynamic_type"
            | "never_type"
    )
}

/// Classify a return type node recursively, tracking nesting and wrapping.
fn classify_return_type<D: Doc>(
    node: &ast_grep_core::Node<'_, D>,
) -> (TypeCategory, u8, bool, bool) {
    let kind = node.kind();
    let kind_str = kind.as_ref();

    match kind_str {
        "type_identifier" => {
            let name = node.text();
            let cat = TypeCategory::classify_rust(name.as_ref());
            let wraps_opt = cat == TypeCategory::Opt;
            let wraps_res = cat == TypeCategory::Res;
            (cat, 0, wraps_opt, wraps_res)
        }
        "primitive_type" => {
            let cat = TypeCategory::classify_rust(node.text().as_ref());
            (cat, 0, false, false)
        }
        "reference_type" => (TypeCategory::Ref, 0, false, false),
        "function_type" => (TypeCategory::Fn, 0, false, false),
        "unit_type" => (TypeCategory::Unknown, 0, false, false),
        "never_type" => (TypeCategory::Unknown, 0, false, false),
        "generic_type" => {
            // e.g. Result<Vec<Item>, Error>
            // The outer type is the first type_identifier child.
            let outer_name = node
                .children()
                .find(|c| c.kind().as_ref() == "type_identifier")
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            let outer_cat = TypeCategory::classify_rust(&outer_name);
            let wraps_opt = outer_cat == TypeCategory::Opt;
            let wraps_res = outer_cat == TypeCategory::Res;

            // Count nesting depth by looking at generic type_arguments.
            let nesting = count_generic_nesting(node);

            (outer_cat, nesting, wraps_opt, wraps_res)
        }
        "scoped_type_identifier" => {
            let text = node.text();
            let text_str = text.as_ref();
            if let Some(last) = text_str.rsplit("::").next() {
                let cat = TypeCategory::classify_rust(last);
                (cat, 0, false, false)
            } else {
                (TypeCategory::Unknown, 0, false, false)
            }
        }
        _ => (TypeCategory::Unknown, 0, false, false),
    }
}

/// Count the nesting depth of generic type arguments.
/// `Result<Vec<Item>, Error>` has depth 2.
fn count_generic_nesting<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> u8 {
    let mut max_depth: u8 = 0;
    for child in node.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == "type_arguments" {
            // This level adds 1; recurse into children for deeper nesting.
            let inner_max = child
                .children()
                .filter(|c| c.kind().as_ref() == "generic_type")
                .map(|c| 1 + count_generic_nesting(&c))
                .max()
                .unwrap_or(1);
            max_depth = max_depth.max(inner_max);
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
            "if_expression" => cf.branches += 1,
            "else_clause" => cf.branches += 1,
            "for_expression" => cf.loops += 1,
            "while_expression" => cf.loops += 1,
            "loop_expression" => cf.loops += 1,
            "return_expression" => cf.early_returns += 1,
            "try_expression" => cf.error_propagations += 1,
            "unsafe_block" => cf.unsafe_blocks += 1,
            "match_arm" => cf.match_arms += 1,
            "closure_expression" => cf.closures += 1,
            "await_expression" => cf.awaits += 1,
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
            "macro_invocation" => sc.calls += 1,
            "assignment_expression" => sc.assignments += 1,
            "let_declaration" => sc.assignments += 1,
            "field_expression" => sc.member_access += 1,
            "index_expression" => sc.index_ops += 1,
            "binary_expression" => sc.binary_ops += 1,
            "array_expression" => sc.collection_literals += 1,
            "closure_expression" => sc.lambdas += 1,
            "type_identifier" => {
                // Count type annotations only in parameter/return positions.
                // We use a heuristic: if the parent is a type-related node,
                // count it.
                if let Some(parent) = descendant.parent() {
                    let pk = parent.kind();
                    let parent_kind = pk.as_ref();
                    if is_type_context(parent_kind) {
                        sc.type_annotations += 1;
                    }
                }
            }
            _ => {}
        }
    }

    sc
}

/// Check if a parent kind indicates a type-annotation context.
fn is_type_context(kind: &str) -> bool {
    matches!(
        kind,
        "generic_type"
            | "reference_type"
            | "type_arguments"
            | "function_type"
            | "tuple_type"
            | "array_type"
            | "pointer_type"
            | "bounded_type"
            | "parameter"
            | "let_declaration"
    )
}
