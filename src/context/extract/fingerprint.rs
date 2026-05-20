pub const FINGERPRINT_DIMS: usize = 64;
pub const MIN_BODY_NODE_COUNT: usize = 10;
pub const MAX_QUERY_SYMBOLS: usize = 8;
pub const STRUCT_BOOST_WEIGHT: f32 = 0.3;
pub const FINGERPRINT_VERSION: &str = "structural-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeCategory {
    Prim,
    Str,
    Col,
    Opt,
    Res,
    Ref,
    Fn,
    SelfRef,
    Unknown,
    Generic,
}

impl TypeCategory {
    pub fn classify_rust(type_name: &str) -> Self {
        match type_name {
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32" | "i64"
            | "i128" | "isize" | "f32" | "f64" | "bool" | "char" => Self::Prim,
            "String" | "str" => Self::Str,
            "Vec" | "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet" | "VecDeque" | "LinkedList"
            | "BinaryHeap" => Self::Col,
            "Option" => Self::Opt,
            "Result" => Self::Res,
            "Fn" | "FnMut" | "FnOnce" => Self::Fn,
            "Self" | "self" => Self::SelfRef,
            _ => Self::Generic,
        }
    }

    pub fn classify_python(type_name: &str) -> Self {
        match type_name {
            "int" | "float" | "bool" | "complex" => Self::Prim,
            "str" | "bytes" => Self::Str,
            "list" | "dict" | "set" | "frozenset" | "tuple" | "List" | "Dict" | "Set" | "Tuple"
            | "Sequence" | "Mapping" => Self::Col,
            "Optional" | "None" => Self::Opt,
            "Callable" => Self::Fn,
            "self" | "cls" => Self::SelfRef,
            "" => Self::Unknown,
            _ => Self::Generic,
        }
    }

    pub fn classify_typescript(type_name: &str) -> Self {
        match type_name {
            "number" | "boolean" | "bigint" => Self::Prim,
            "string" => Self::Str,
            "Array" | "Map" | "Set" | "WeakMap" | "WeakSet" => Self::Col,
            "Promise" => Self::Res,
            "Function" => Self::Fn,
            "this" => Self::SelfRef,
            "undefined" | "null" | "void" => Self::Opt,
            "unknown" | "any" | "never" => Self::Unknown,
            _ => Self::Generic,
        }
    }

    pub fn classify_go(name: &str) -> Self {
        match name {
            "error" => Self::Res,
            "bool" => Self::Prim,
            "int" | "int8" | "int16" | "int32" | "int64" => Self::Prim,
            "uint" | "uint8" | "uint16" | "uint32" | "uint64" => Self::Prim,
            "float32" | "float64" => Self::Prim,
            "complex64" | "complex128" => Self::Prim,
            "string" => Self::Str,
            "byte" | "rune" => Self::Prim,
            _ => Self::Generic,
        }
    }

    fn dim_index(&self) -> usize {
        match self {
            Self::Prim => 0,
            Self::Str => 1,
            Self::Col => 2,
            Self::Opt => 3,
            Self::Res => 4,
            Self::Ref => 5,
            Self::Fn => 6,
            Self::Unknown | Self::Generic | Self::SelfRef => 7,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SignatureShape {
    pub arity: usize,
    pub has_self: bool,
    pub is_mut_self: bool,
    pub is_method: bool,
    pub is_static: bool,
    pub is_constructor: bool,
    pub param_categories: Vec<TypeCategory>,
    pub return_category: Option<TypeCategory>,
    pub return_nesting: u8,
    pub return_wraps_option: bool,
    pub return_wraps_result: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ControlFlowSketch {
    pub branches: u32,
    pub loops: u32,
    pub early_returns: u32,
    pub error_propagations: u32,
    pub unsafe_blocks: u32,
    pub match_arms: u32,
    pub closures: u32,
    pub awaits: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SemanticCounts {
    pub calls: u32,
    pub assignments: u32,
    pub member_access: u32,
    pub index_ops: u32,
    pub binary_ops: u32,
    pub collection_literals: u32,
    pub type_annotations: u32,
    pub lambdas: u32,
}

#[derive(Debug, Clone)]
pub struct StructuralFingerprint {
    pub signature: SignatureShape,
    pub control_flow: ControlFlowSketch,
    pub semantic_counts: SemanticCounts,
}

impl StructuralFingerprint {
    pub fn to_vector(&self) -> [f32; FINGERPRINT_DIMS] {
        let mut v = [0.0f32; FINGERPRINT_DIMS];

        // Dims 0-7: Signature shape (arity + param category histogram).
        // Param histogram occupies dims 1-7 (7 buckets); dim_index is
        // clamped to 6 so Unknown/Generic/SelfRef share the Fn bucket,
        // keeping dims 8-15 free for the return type one-hot.
        v[0] = (self.signature.arity as f32).min(20.0) / 20.0;
        let total_params = self.signature.param_categories.len().max(1) as f32;
        for cat in &self.signature.param_categories {
            v[1 + cat.dim_index().min(6)] += 1.0 / total_params;
        }

        // Dims 8-15: Return type category one-hot (8 slots for TypeCategory)
        if let Some(ret) = &self.signature.return_category {
            v[8 + ret.dim_index()] = 1.0;
        }

        // Dims 16-20: Self/receiver flags
        v[16] = if self.signature.has_self { 1.0 } else { 0.0 };
        v[17] = if self.signature.is_mut_self { 1.0 } else { 0.0 };
        v[18] = if self.signature.is_static { 1.0 } else { 0.0 };
        v[19] = if self.signature.is_method { 1.0 } else { 0.0 };
        v[20] = if self.signature.is_constructor {
            1.0
        } else {
            0.0
        };

        // Dims 21-23: Return type nesting + wrapping (separate from one-hot)
        v[21] = (self.signature.return_nesting as f32).min(3.0) / 3.0;
        v[22] = if self.signature.return_wraps_result {
            0.5
        } else {
            0.0
        } + if self.signature.return_wraps_option {
            0.5
        } else {
            0.0
        };

        // Dims 24-39: First 4 params positionally (4 dims each).
        // Only 4 slots per position so dim_index is clamped: Prim/Str/Col/Opt
        // each get a unique slot; Res/Ref/Fn/SelfRef/Unknown/Generic share slot 3.
        for (i, cat) in self.signature.param_categories.iter().take(4).enumerate() {
            let base = 24 + i * 4;
            let idx = cat.dim_index().min(3);
            v[base + idx] = 1.0;
        }

        // Dims 40-47: Global shape
        let cf = &self.control_flow;
        let sc = &self.semantic_counts;
        let total_nodes = [
            cf.branches,
            cf.loops,
            cf.early_returns,
            cf.error_propagations,
            cf.unsafe_blocks,
            cf.match_arms,
            cf.closures,
            cf.awaits,
            sc.calls,
            sc.assignments,
            sc.member_access,
            sc.index_ops,
            sc.binary_ops,
            sc.collection_literals,
            sc.type_annotations,
            sc.lambdas,
        ]
        .iter()
        .fold(0u32, |acc, &x| acc.saturating_add(x));
        v[40] = (total_nodes as f32).ln_1p() / 10.0;
        // dims 41-47 reserved for max_depth, mean_depth, leaf_ratio etc.
        // (populated by the language-specific fingerprinter if available)

        // Dims 48-55: Control-flow sketch (log1p, per-family normalized)
        let cf_vals = [
            cf.branches,
            cf.loops,
            cf.early_returns,
            cf.error_propagations,
            cf.unsafe_blocks,
            cf.match_arms,
            cf.closures,
            cf.awaits,
        ];
        let cf_max = cf_vals.iter().copied().max().unwrap_or(1).max(1) as f32;
        for (i, &val) in cf_vals.iter().enumerate() {
            v[48 + i] = (val as f32).ln_1p() / cf_max.ln_1p().max(f32::EPSILON);
        }

        // Dims 56-63: Semantic counts (log1p, per-family normalized)
        let sc_vals = [
            sc.calls,
            sc.assignments,
            sc.member_access,
            sc.index_ops,
            sc.binary_ops,
            sc.collection_literals,
            sc.type_annotations,
            sc.lambdas,
        ];
        let sc_max = sc_vals.iter().copied().max().unwrap_or(1).max(1) as f32;
        for (i, &val) in sc_vals.iter().enumerate() {
            v[56 + i] = (val as f32).ln_1p() / sc_max.ln_1p().max(f32::EPSILON);
        }

        v
    }
}

pub fn cosine_similarity(a: &[f32; FINGERPRINT_DIMS], b: &[f32; FINGERPRINT_DIMS]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for i in 0..FINGERPRINT_DIMS {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < f32::EPSILON {
        0.0
    } else {
        dot / denom
    }
}
