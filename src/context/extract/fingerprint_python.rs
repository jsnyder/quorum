//! Python AST structural fingerprinter.
//!
//! Walks a tree-sitter parse tree (via ast-grep) to extract structural features
//! from Python functions and methods, producing a [`StructuralFingerprint`] that
//! can be projected into a fixed-size vector for similarity search.

use ast_grep_core::Doc;
use ast_grep_language::{LanguageExt, SupportLang};

use super::fingerprint::{
    ControlFlowSketch, SemanticCounts, SignatureShape, StructuralFingerprint, TypeCategory,
    MIN_BODY_NODE_COUNT,
};

/// Stateless fingerprinter for Python source code.
pub struct PythonFingerprinter;

impl PythonFingerprinter {
    /// Convenience entry point for testing: parse `src`, find the first
    /// `function_definition` node, and fingerprint it.
    pub fn fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint> {
        let root = SupportLang::Python.ast_grep(src);
        let root_node = root.root();
        let func_node = find_first_function(&root_node)?;
        self.fingerprint_node(&func_node, src)
    }

    /// Extract a [`StructuralFingerprint`] from a function/method AST node.
    ///
    /// Returns `None` if the function body has fewer than [`MIN_BODY_NODE_COUNT`]
    /// descendant nodes (trivial function filter).
    pub fn fingerprint_node<'a, D: Doc>(
        &self,
        node: &'a ast_grep_core::Node<'a, D>,
        source: &str,
    ) -> Option<StructuralFingerprint> {
        let kind = node.kind();
        let kind_str = kind.as_ref();
        if kind_str != "function_definition" {
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
        let semantic_counts = extract_semantic_counts(&body, source);

        Some(StructuralFingerprint {
            signature,
            control_flow,
            semantic_counts,
        })
    }
}

/// Find the first `function_definition` in a DFS walk of the tree.
fn find_first_function<'a, D: Doc>(
    root: &'a ast_grep_core::Node<'a, D>,
) -> Option<ast_grep_core::Node<'a, D>> {
    root.dfs()
        .find(|n| n.kind().as_ref() == "function_definition")
}

/// Extract signature shape from a function/method node.
fn extract_signature<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    source: &str,
) -> SignatureShape {
    let mut shape = SignatureShape::default();

    // Detect if inside a class by walking ancestors.
    let in_class = node
        .ancestors()
        .any(|a| a.kind().as_ref() == "class_definition");

    // Find the parameters node.
    if let Some(params) = node.children().find(|c| c.kind().as_ref() == "parameters") {
        extract_params(&params, &mut shape, source, in_class);
    }

    // Extract return type annotation (-> type).
    extract_return_type(node, &mut shape, source);

    shape.is_method = in_class && shape.has_self;
    shape.is_static = in_class && !shape.has_self;

    // Constructor heuristic: method named __init__.
    if in_class && shape.has_self {
        if let Some(name_node) = node.children().find(|c| c.kind().as_ref() == "identifier") {
            if name_node.text().as_ref() == "__init__" {
                shape.is_constructor = true;
            }
        }
    }

    shape
}

/// Extract parameters from a `parameters` node, populating `shape`.
///
/// In Python, `self` and `cls` as the first parameter indicate a method or
/// classmethod respectively. These are excluded from param_categories and
/// arity but set `has_self`.
fn extract_params<D: Doc>(
    params: &ast_grep_core::Node<'_, D>,
    shape: &mut SignatureShape,
    source: &str,
    in_class: bool,
) {
    let mut is_first = true;
    for child in params.children() {
        let ck = child.kind();
        let child_kind = ck.as_ref();
        match child_kind {
            "identifier" => {
                let name = child.text();
                let name_str = name.as_ref();
                // self/cls as first param in a class method
                if is_first && in_class && (name_str == "self" || name_str == "cls") {
                    shape.has_self = true;
                    is_first = false;
                    continue;
                }
                is_first = false;
                // Bare identifier param (no type annotation)
                shape.arity += 1;
                shape.param_categories.push(TypeCategory::Unknown);
            }
            "typed_parameter" => {
                let param_name = child
                    .children()
                    .find(|c| c.kind().as_ref() == "identifier")
                    .map(|c| c.text().into_owned())
                    .unwrap_or_default();
                // self/cls as first param with type annotation (rare but valid)
                if is_first && in_class && (param_name == "self" || param_name == "cls") {
                    shape.has_self = true;
                    is_first = false;
                    continue;
                }
                is_first = false;
                shape.arity += 1;
                let cat = classify_typed_param(&child, source);
                shape.param_categories.push(cat);
            }
            "default_parameter" => {
                // `param=value` or `param: type = value`
                let param_name = child
                    .children()
                    .find(|c| {
                        let k = c.kind();
                        let ks = k.as_ref();
                        ks == "identifier" || ks == "typed_parameter"
                    })
                    .map(|c| {
                        if c.kind().as_ref() == "typed_parameter" {
                            // Extract name from the typed_parameter
                            c.children()
                                .find(|cc| cc.kind().as_ref() == "identifier")
                                .map(|cc| cc.text().into_owned())
                                .unwrap_or_default()
                        } else {
                            c.text().into_owned()
                        }
                    })
                    .unwrap_or_default();
                if is_first && in_class && (param_name == "self" || param_name == "cls") {
                    shape.has_self = true;
                    is_first = false;
                    continue;
                }
                is_first = false;
                shape.arity += 1;
                // Check if this default_parameter contains a typed_parameter child
                let cat = if let Some(typed) = child
                    .children()
                    .find(|c| c.kind().as_ref() == "typed_parameter")
                {
                    classify_typed_param(&typed, source)
                } else {
                    TypeCategory::Unknown
                };
                shape.param_categories.push(cat);
            }
            "typed_default_parameter" => {
                // `param: type = value` (some tree-sitter grammars use this)
                let param_name = child
                    .children()
                    .find(|c| c.kind().as_ref() == "identifier")
                    .map(|c| c.text().into_owned())
                    .unwrap_or_default();
                if is_first && in_class && (param_name == "self" || param_name == "cls") {
                    shape.has_self = true;
                    is_first = false;
                    continue;
                }
                is_first = false;
                shape.arity += 1;
                let cat = classify_type_node(&child, source);
                shape.param_categories.push(cat);
            }
            "list_splat_pattern" | "dictionary_splat_pattern" => {
                // *args / **kwargs
                is_first = false;
                shape.arity += 1;
                shape.param_categories.push(TypeCategory::Col);
            }
            _ => {}
        }
    }
}

/// Classify a typed parameter's type annotation. Looks for a `type` child
/// within the `typed_parameter` node.
fn classify_typed_param<D: Doc>(
    param: &ast_grep_core::Node<'_, D>,
    source: &str,
) -> TypeCategory {
    // In tree-sitter-python, a typed_parameter has: identifier, ":", type
    // The type child might be an `identifier`, `attribute`, `subscript`,
    // `generic_type`, etc.
    if let Some(type_node) = param.children().find(|c| c.kind().as_ref() == "type") {
        return classify_type_expression(&type_node, source);
    }
    // Fallback: scan children for type-like nodes after the colon
    let mut saw_colon = false;
    for child in param.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == ":" {
            saw_colon = true;
            continue;
        }
        if saw_colon {
            return classify_type_expression(&child, source);
        }
    }
    TypeCategory::Unknown
}

/// Classify a type node from a `type` wrapper or bare type expression.
fn classify_type_node<D: Doc>(
    param: &ast_grep_core::Node<'_, D>,
    source: &str,
) -> TypeCategory {
    if let Some(type_node) = param.children().find(|c| c.kind().as_ref() == "type") {
        return classify_type_expression(&type_node, source);
    }
    // For typed_default_parameter: name : type = default
    // Look for a type expression after the colon
    let mut saw_colon = false;
    for child in param.children() {
        let ck = child.kind();
        let kind = ck.as_ref();
        if kind == ":" {
            saw_colon = true;
            continue;
        }
        if saw_colon && kind != "=" {
            return classify_type_expression(&child, source);
        }
        if kind == "=" {
            break;
        }
    }
    TypeCategory::Unknown
}

/// Classify a type expression node (identifier, attribute, subscript, etc.)
/// using the Python type classifier.
fn classify_type_expression<D: Doc>(
    node: &ast_grep_core::Node<'_, D>,
    _source: &str,
) -> TypeCategory {
    let kind = node.kind();
    let kind_str = kind.as_ref();
    match kind_str {
        "identifier" => TypeCategory::classify_python(node.text().as_ref()),
        "type" => {
            // Unwrap the `type` wrapper node — classify its first child.
            if let Some(inner) = node.children().next() {
                return classify_type_expression(&inner, _source);
            }
            TypeCategory::Unknown
        }
        "attribute" => {
            // e.g. typing.List — use the last identifier segment
            let text = node.text();
            let text_str = text.as_ref();
            if let Some(last) = text_str.rsplit('.').next() {
                TypeCategory::classify_python(last)
            } else {
                TypeCategory::Generic
            }
        }
        "subscript" => {
            // e.g. List[int], Optional[str], Dict[str, int]
            // Classify the outer type (the base before [])
            if let Some(base) = node.children().next() {
                return classify_type_expression(&base, _source);
            }
            TypeCategory::Generic
        }
        "none" => TypeCategory::Opt,
        _ => TypeCategory::Unknown,
    }
}

/// Extract the return type annotation from a function node.
/// In tree-sitter-python, the return type is after `->` in the function
/// definition.
fn extract_return_type<'a, D: Doc>(
    node: &'a ast_grep_core::Node<'a, D>,
    shape: &mut SignatureShape,
    source: &str,
) {
    // Try the field-based approach first
    let ret_type = node.field("return_type").or_else(|| {
        // Fallback: scan children for a type node after `->`
        let mut saw_arrow = false;
        for child in node.children() {
            let ck = child.kind();
            let kind = ck.as_ref();
            if kind == "->" {
                saw_arrow = true;
                continue;
            }
            if saw_arrow {
                return Some(child);
            }
        }
        None
    });

    let Some(ret) = ret_type else {
        return;
    };

    let cat = classify_type_expression(&ret, source);
    shape.return_category = Some(cat);

    // Check for nesting and wrapping
    let ck = ret.kind();
    let kind_str = ck.as_ref();
    if kind_str == "subscript" || kind_str == "type" {
        let inner = if kind_str == "type" {
            ret.children().next()
        } else {
            Some(ret.clone())
        };
        if let Some(sub_node) = inner {
            if sub_node.kind().as_ref() == "subscript" {
                let base_text = sub_node
                    .children()
                    .next()
                    .map(|c| c.text().into_owned())
                    .unwrap_or_default();
                let base_cat = TypeCategory::classify_python(&base_text);
                shape.return_wraps_option = base_cat == TypeCategory::Opt;
                // Python has no native Result type, but Optional is common
                shape.return_nesting = count_subscript_nesting(&sub_node);
            }
        }
    }
}

/// Count nesting depth of subscript type expressions.
/// `Optional[List[Dict[str, int]]]` has depth 3.
fn count_subscript_nesting<D: Doc>(node: &ast_grep_core::Node<'_, D>) -> u8 {
    let mut max_depth: u8 = 0;
    for child in node.children() {
        if child.kind().as_ref() == "subscript" {
            let inner = 1 + count_subscript_nesting(&child);
            max_depth = max_depth.max(inner);
        }
    }
    if max_depth == 0 && node.kind().as_ref() == "subscript" {
        // The current subscript itself counts as depth 1
        return 1;
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
            "elif_clause" => cf.branches += 1,
            "else_clause" => cf.branches += 1,
            "for_statement" => cf.loops += 1,
            "while_statement" => cf.loops += 1,
            "try_statement" => cf.error_propagations += 1,
            "except_clause" => cf.error_propagations += 1,
            "with_statement" => cf.branches += 1,
            "raise_statement" => cf.early_returns += 1,
            "return_statement" => cf.early_returns += 1,
            "yield" => cf.early_returns += 1,
            "await" => cf.awaits += 1,
            "lambda" => cf.closures += 1,
            "list_comprehension" | "dict_comprehension" | "set_comprehension"
            | "generator_expression" => {
                cf.loops += 1;
            }
            _ => {}
        }
    }

    cf
}

/// Extract semantic counts from a function body node.
fn extract_semantic_counts<D: Doc>(
    body: &ast_grep_core::Node<'_, D>,
    _source: &str,
) -> SemanticCounts {
    let mut sc = SemanticCounts::default();

    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "call" => sc.calls += 1,
            "assignment" | "augmented_assignment" => sc.assignments += 1,
            "attribute" => sc.member_access += 1,
            "subscript" => {
                // Only count subscript as index_op when NOT in a type context.
                // Type annotations like `List[int]` use subscript syntax but
                // are not index operations.
                let is_type_ctx = descendant
                    .parent()
                    .map(|p| {
                        let pk = p.kind();
                        let parent_kind = pk.as_ref();
                        parent_kind == "type"
                            || parent_kind == "return_type"
                            || parent_kind == "typed_parameter"
                            || parent_kind == "typed_default_parameter"
                    })
                    .unwrap_or(false);
                if !is_type_ctx {
                    sc.index_ops += 1;
                }
            }
            "binary_operator" | "boolean_operator" | "comparison_operator"
            | "not_operator" => sc.binary_ops += 1,
            "list_comprehension" | "dict_comprehension" | "set_comprehension"
            | "generator_expression" => {
                sc.collection_literals += 1;
            }
            "list" | "dictionary" | "set" => sc.collection_literals += 1,
            "lambda" => sc.lambdas += 1,
            "type" => sc.type_annotations += 1,
            _ => {}
        }
    }

    sc
}
