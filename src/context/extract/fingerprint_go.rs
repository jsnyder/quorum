use ast_grep_core::Doc;
use ast_grep_language::{LanguageExt, SupportLang};

use super::fingerprint::{
    ControlFlowSketch, MIN_BODY_NODE_COUNT, SemanticCounts, SignatureShape, StructuralFingerprint,
    TypeCategory,
};

pub struct GoFingerprinter;

impl GoFingerprinter {
    pub fn fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint> {
        let root = SupportLang::Go.ast_grep(src);
        let root_node = root.root();
        let func_node = root_node.dfs().find(|n| {
            let k = n.kind();
            k.as_ref() == "function_declaration" || k.as_ref() == "method_declaration"
        })?;
        self.fingerprint_node(&func_node, src)
    }

    pub fn fingerprint_all_functions(&self, src: &str) -> Vec<(String, StructuralFingerprint)> {
        let root = SupportLang::Go.ast_grep(src);
        let root_node = root.root();
        let func_nodes: Vec<_> = root_node
            .dfs()
            .filter(|n| {
                let k = n.kind();
                k.as_ref() == "function_declaration" || k.as_ref() == "method_declaration"
            })
            .collect();
        let mut results = Vec::new();
        for node in &func_nodes {
            let name = node
                .children()
                .find(|c| {
                    c.kind().as_ref() == "identifier" || c.kind().as_ref() == "field_identifier"
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
        _source: &str,
    ) -> Option<StructuralFingerprint> {
        let kind = node.kind();
        let kind_str = kind.as_ref();
        if kind_str != "function_declaration" && kind_str != "method_declaration" {
            return None;
        }

        let body = node.children().find(|c| c.kind().as_ref() == "block")?;
        let body_node_count = body.dfs().count();
        if body_node_count < MIN_BODY_NODE_COUNT {
            return None;
        }

        let signature = extract_go_signature(node);
        let control_flow = extract_go_control_flow(&body);
        let semantic_counts = extract_go_semantic_counts(&body);

        Some(StructuralFingerprint {
            signature,
            control_flow,
            semantic_counts,
        })
    }
}

fn extract_go_signature<'a, D: Doc>(node: &'a ast_grep_core::Node<'a, D>) -> SignatureShape {
    let mut shape = SignatureShape::default();
    let kind = node.kind();
    shape.is_method = kind.as_ref() == "method_declaration";

    let mut param_lists: Vec<_> = node
        .children()
        .filter(|c| c.kind().as_ref() == "parameter_list")
        .collect();

    // For methods, the first parameter_list is the receiver; skip it
    if shape.is_method && param_lists.len() > 1 {
        param_lists.remove(0);
    }

    if let Some(params) = param_lists.first() {
        shape.arity = params
            .children()
            .filter(|c| c.kind().as_ref() == "parameter_declaration")
            .count();
    }

    // Check for result type (simple single return)
    if let Some(result) = node
        .children()
        .find(|c| c.kind().as_ref() == "type_identifier")
    {
        let text = result.text();
        shape.return_category = Some(TypeCategory::classify_go(text.as_ref()));
    }

    shape
}

fn extract_go_control_flow<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> ControlFlowSketch {
    let mut cf = ControlFlowSketch::default();
    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "if_statement" => cf.branches += 1,
            "for_statement" => cf.loops += 1,
            "return_statement" => cf.early_returns += 1,
            "go_statement" => cf.awaits += 1,
            "defer_statement" => cf.closures += 1,
            "select_statement" | "expression_case" | "type_case" => cf.match_arms += 1,
            _ => {}
        }
    }
    cf
}

fn extract_go_semantic_counts<D: Doc>(body: &ast_grep_core::Node<'_, D>) -> SemanticCounts {
    let mut sc = SemanticCounts::default();
    for descendant in body.dfs() {
        let dk = descendant.kind();
        let kind = dk.as_ref();
        match kind {
            "call_expression" => sc.calls += 1,
            "assignment_statement" | "short_var_declaration" => sc.assignments += 1,
            "selector_expression" => sc.member_access += 1,
            "index_expression" => sc.index_ops += 1,
            "binary_expression" => sc.binary_ops += 1,
            "composite_literal" => sc.collection_literals += 1,
            "func_literal" => sc.lambdas += 1,
            _ => {}
        }
    }
    sc
}
