use super::fingerprint::*;

#[test]
fn type_category_classify_rust_primitives() {
    assert_eq!(TypeCategory::classify_rust("u32"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("bool"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("f64"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("usize"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("i128"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("char"), TypeCategory::Prim);
}

#[test]
fn type_category_classify_rust_strings() {
    assert_eq!(TypeCategory::classify_rust("String"), TypeCategory::Str);
    assert_eq!(TypeCategory::classify_rust("str"), TypeCategory::Str);
}

#[test]
fn type_category_classify_rust_collections() {
    assert_eq!(TypeCategory::classify_rust("Vec"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_rust("HashMap"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_rust("BTreeMap"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_rust("HashSet"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_rust("VecDeque"), TypeCategory::Col);
}

#[test]
fn type_category_classify_rust_result_option() {
    assert_eq!(TypeCategory::classify_rust("Result"), TypeCategory::Res);
    assert_eq!(TypeCategory::classify_rust("Option"), TypeCategory::Opt);
}

#[test]
fn type_category_classify_rust_fn_traits() {
    assert_eq!(TypeCategory::classify_rust("Fn"), TypeCategory::Fn);
    assert_eq!(TypeCategory::classify_rust("FnMut"), TypeCategory::Fn);
    assert_eq!(TypeCategory::classify_rust("FnOnce"), TypeCategory::Fn);
}

#[test]
fn type_category_classify_rust_user_type_is_generic() {
    assert_eq!(TypeCategory::classify_rust("MyStruct"), TypeCategory::Generic);
    assert_eq!(TypeCategory::classify_rust("Config"), TypeCategory::Generic);
}

#[test]
fn type_category_classify_python_primitives() {
    assert_eq!(TypeCategory::classify_python("int"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_python("float"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_python("bool"), TypeCategory::Prim);
}

#[test]
fn type_category_classify_python_collections() {
    assert_eq!(TypeCategory::classify_python("list"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_python("dict"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_python("List"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_python("Dict"), TypeCategory::Col);
}

#[test]
fn type_category_classify_python_untyped_is_unknown() {
    assert_eq!(TypeCategory::classify_python(""), TypeCategory::Unknown);
}

#[test]
fn type_category_classify_python_self_cls() {
    assert_eq!(TypeCategory::classify_python("self"), TypeCategory::SelfRef);
    assert_eq!(TypeCategory::classify_python("cls"), TypeCategory::SelfRef);
}

#[test]
fn type_category_classify_typescript_primitives() {
    assert_eq!(TypeCategory::classify_typescript("number"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_typescript("boolean"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_typescript("bigint"), TypeCategory::Prim);
}

#[test]
fn type_category_classify_typescript_string() {
    assert_eq!(TypeCategory::classify_typescript("string"), TypeCategory::Str);
}

#[test]
fn type_category_classify_typescript_promise_is_result() {
    assert_eq!(TypeCategory::classify_typescript("Promise"), TypeCategory::Res);
}

#[test]
fn type_category_classify_typescript_any_unknown_is_unknown() {
    assert_eq!(TypeCategory::classify_typescript("any"), TypeCategory::Unknown);
    assert_eq!(TypeCategory::classify_typescript("unknown"), TypeCategory::Unknown);
}

#[test]
fn fingerprint_to_vector_has_64_dims() {
    let fp = StructuralFingerprint {
        signature: SignatureShape {
            arity: 3,
            has_self: true,
            is_mut_self: false,
            is_method: true,
            is_static: false,
            is_constructor: false,
            param_categories: vec![TypeCategory::Ref, TypeCategory::Col, TypeCategory::Prim],
            return_category: Some(TypeCategory::Res),
            return_nesting: 1,
            return_wraps_option: false,
            return_wraps_result: true,
        },
        control_flow: ControlFlowSketch {
            branches: 2,
            loops: 1,
            early_returns: 1,
            error_propagations: 3,
            unsafe_blocks: 0,
            match_arms: 0,
            closures: 0,
            awaits: 0,
        },
        semantic_counts: SemanticCounts {
            calls: 5,
            assignments: 2,
            member_access: 4,
            index_ops: 0,
            binary_ops: 3,
            collection_literals: 1,
            type_annotations: 2,
            lambdas: 0,
        },
    };
    let vec = fp.to_vector();
    assert_eq!(vec.len(), 64);
    assert!(vec.iter().all(|v| v.is_finite()));
    // Arity dim should be nonzero
    assert!(vec[0] > 0.0, "arity should be encoded in dim 0");
}

#[test]
fn fingerprint_to_vector_is_deterministic() {
    let fp = StructuralFingerprint {
        signature: SignatureShape {
            arity: 2,
            has_self: false,
            is_mut_self: false,
            is_method: false,
            is_static: true,
            is_constructor: false,
            param_categories: vec![TypeCategory::Str, TypeCategory::Prim],
            return_category: Some(TypeCategory::Col),
            return_nesting: 0,
            return_wraps_option: false,
            return_wraps_result: false,
        },
        control_flow: ControlFlowSketch::default(),
        semantic_counts: SemanticCounts::default(),
    };
    let v1 = fp.to_vector();
    let v2 = fp.to_vector();
    assert_eq!(v1, v2);
}

#[test]
fn fingerprint_to_vector_self_receiver_flags() {
    let fp = StructuralFingerprint {
        signature: SignatureShape {
            arity: 0,
            has_self: true,
            is_mut_self: true,
            is_method: true,
            is_static: false,
            is_constructor: false,
            param_categories: vec![],
            return_category: None,
            return_nesting: 0,
            return_wraps_option: false,
            return_wraps_result: false,
        },
        control_flow: ControlFlowSketch::default(),
        semantic_counts: SemanticCounts::default(),
    };
    let vec = fp.to_vector();
    assert_eq!(vec[16], 1.0, "has_self");
    assert_eq!(vec[17], 1.0, "is_mut_self");
    assert_eq!(vec[18], 0.0, "is_static");
    assert_eq!(vec[19], 1.0, "is_method");
    assert_eq!(vec[20], 0.0, "is_constructor");
}

#[test]
fn fingerprint_to_vector_return_type_encoded() {
    let fp = StructuralFingerprint {
        signature: SignatureShape {
            arity: 0,
            has_self: false,
            is_mut_self: false,
            is_method: false,
            is_static: true,
            is_constructor: false,
            param_categories: vec![],
            return_category: Some(TypeCategory::Col),
            return_nesting: 2,
            return_wraps_option: true,
            return_wraps_result: true,
        },
        control_flow: ControlFlowSketch::default(),
        semantic_counts: SemanticCounts::default(),
    };
    let vec = fp.to_vector();
    // Col is dim_index 2, so dims[8+2] = 1.0
    assert_eq!(vec[10], 1.0, "return category Col at dim 10");
    assert!(vec[21] > 0.0, "return nesting should be nonzero");
    assert_eq!(vec[22], 1.0, "both option and result wrapping = 0.5 + 0.5");
}

#[test]
fn fingerprint_to_vector_param_positional_encoding() {
    let fp = StructuralFingerprint {
        signature: SignatureShape {
            arity: 2,
            has_self: false,
            is_mut_self: false,
            is_method: false,
            is_static: true,
            is_constructor: false,
            param_categories: vec![TypeCategory::Str, TypeCategory::Col],
            return_category: None,
            return_nesting: 0,
            return_wraps_option: false,
            return_wraps_result: false,
        },
        control_flow: ControlFlowSketch::default(),
        semantic_counts: SemanticCounts::default(),
    };
    let vec = fp.to_vector();
    // Param 0 (Str, dim_index=1): dims[24 + 0*4 + min(1,3)] = dims[25] = 1.0
    assert_eq!(vec[25], 1.0, "first param Str at positional dim 25");
    // Param 1 (Col, dim_index=2): dims[24 + 1*4 + min(2,3)] = dims[30] = 1.0
    assert_eq!(vec[30], 1.0, "second param Col at positional dim 30");
}

#[test]
fn cosine_similarity_identical_vectors() {
    let a = [1.0f32; FINGERPRINT_DIMS];
    let sim = cosine_similarity(&a, &a);
    assert!((sim - 1.0).abs() < 1e-5);
}

#[test]
fn cosine_similarity_orthogonal_vectors() {
    let mut a = [0.0f32; FINGERPRINT_DIMS];
    let mut b = [0.0f32; FINGERPRINT_DIMS];
    a[0] = 1.0;
    b[1] = 1.0;
    let sim = cosine_similarity(&a, &b);
    assert!(sim.abs() < 1e-5);
}

#[test]
fn cosine_similarity_zero_vector_returns_zero() {
    let a = [0.0f32; FINGERPRINT_DIMS];
    let b = [1.0f32; FINGERPRINT_DIMS];
    let sim = cosine_similarity(&a, &b);
    assert_eq!(sim, 0.0);
}

#[test]
fn min_body_node_count_constant() {
    assert_eq!(MIN_BODY_NODE_COUNT, 10);
}

#[test]
fn max_query_symbols_constant() {
    assert_eq!(MAX_QUERY_SYMBOLS, 8);
}

#[test]
fn fingerprint_dims_constant() {
    assert_eq!(FINGERPRINT_DIMS, 64);
}

#[test]
fn struct_boost_weight_constant() {
    assert!((STRUCT_BOOST_WEIGHT - 0.3).abs() < 1e-5);
}

#[test]
fn default_control_flow_sketch_is_zero() {
    let cf = ControlFlowSketch::default();
    assert_eq!(cf.branches, 0);
    assert_eq!(cf.loops, 0);
    assert_eq!(cf.early_returns, 0);
    assert_eq!(cf.error_propagations, 0);
    assert_eq!(cf.unsafe_blocks, 0);
    assert_eq!(cf.match_arms, 0);
    assert_eq!(cf.closures, 0);
    assert_eq!(cf.awaits, 0);
}

#[test]
fn default_semantic_counts_is_zero() {
    let sc = SemanticCounts::default();
    assert_eq!(sc.calls, 0);
    assert_eq!(sc.assignments, 0);
    assert_eq!(sc.member_access, 0);
    assert_eq!(sc.index_ops, 0);
    assert_eq!(sc.binary_ops, 0);
    assert_eq!(sc.collection_literals, 0);
    assert_eq!(sc.type_annotations, 0);
    assert_eq!(sc.lambdas, 0);
}

#[test]
fn to_vector_saturates_on_large_counts() {
    let fp = StructuralFingerprint {
        signature: SignatureShape::default(),
        control_flow: ControlFlowSketch {
            branches: u32::MAX / 2,
            loops: u32::MAX / 2,
            early_returns: u32::MAX / 2,
            ..ControlFlowSketch::default()
        },
        semantic_counts: SemanticCounts {
            calls: u32::MAX / 2,
            assignments: u32::MAX / 2,
            ..SemanticCounts::default()
        },
    };
    let vec = fp.to_vector();
    assert!(vec.iter().all(|v| v.is_finite()), "overflow produced non-finite values");
    assert!(vec[40] > 0.0, "global shape dim should be positive");
}

#[test]
fn empty_fingerprint_produces_finite_vector() {
    let fp = StructuralFingerprint {
        signature: SignatureShape::default(),
        control_flow: ControlFlowSketch::default(),
        semantic_counts: SemanticCounts::default(),
    };
    let vec = fp.to_vector();
    assert_eq!(vec.len(), 64);
    assert!(vec.iter().all(|v| v.is_finite()));
}
