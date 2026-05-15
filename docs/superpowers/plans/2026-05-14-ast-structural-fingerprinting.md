# AST Structural Fingerprinting Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fourth retrieval leg that finds structurally similar code across indexed repos using a 64-dim fingerprint vector computed from AST structure.

**Architecture:** Fingerprints are computed at index time by per-language `Fingerprinter` implementations, stored in a new `chunks_struct_vec` sqlite-vec table, and queried via KNN as a fourth retrieval leg. The structural signal enters the reranker as an additive boost (not a blend weight change), preserving existing BM25/vector quality.

**Tech Stack:** Rust, tree-sitter (via ast-grep's `SupportLang`), sqlite-vec, rusqlite

---

### Task 1: Core Types and Fingerprinter Trait

**Files:**
- Create: `src/context/extract/fingerprint.rs`
- Modify: `src/context/extract/mod.rs` — add `pub mod fingerprint;`
- Test: `src/context/extract/fingerprint_tests.rs`

- [ ] **Step 1: Write failing test for TypeCategory and to_vector**

```rust
// src/context/extract/fingerprint_tests.rs
use super::fingerprint::*;

#[test]
fn type_category_from_str_primitives() {
    assert_eq!(TypeCategory::classify_rust("u32"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("bool"), TypeCategory::Prim);
    assert_eq!(TypeCategory::classify_rust("f64"), TypeCategory::Prim);
}

#[test]
fn type_category_from_str_collections() {
    assert_eq!(TypeCategory::classify_rust("Vec"), TypeCategory::Col);
    assert_eq!(TypeCategory::classify_rust("HashMap"), TypeCategory::Col);
}

#[test]
fn type_category_from_str_result_option() {
    assert_eq!(TypeCategory::classify_rust("Result"), TypeCategory::Res);
    assert_eq!(TypeCategory::classify_rust("Option"), TypeCategory::Opt);
}

#[test]
fn type_category_unknown_user_type() {
    assert_eq!(TypeCategory::classify_rust("MyStruct"), TypeCategory::Generic);
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
    // All values should be finite
    assert!(vec.iter().all(|v| v.is_finite()));
    // Arity dim should be nonzero (normalized arity of 3)
    assert!(vec[0] > 0.0);
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
fn min_body_complexity_constant() {
    assert_eq!(MIN_BODY_NODE_COUNT, 10);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum fingerprint_tests -- --nocapture 2>&1 | head -30`
Expected: FAIL — module not found

- [ ] **Step 3: Implement core types**

```rust
// src/context/extract/fingerprint.rs

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
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
            | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "f32" | "f64" | "bool" | "char" => Self::Prim,
            "String" | "str" | "&str" => Self::Str,
            "Vec" | "HashMap" | "BTreeMap" | "HashSet" | "BTreeSet"
            | "VecDeque" | "LinkedList" | "BinaryHeap" => Self::Col,
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
            "list" | "dict" | "set" | "frozenset" | "tuple"
            | "List" | "Dict" | "Set" | "Tuple" | "Sequence" | "Mapping" => Self::Col,
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

    fn dim_index(&self) -> usize {
        match self {
            Self::Prim => 0,
            Self::Str => 1,
            Self::Col => 2,
            Self::Opt => 3,
            Self::Res => 4,
            Self::Ref => 5,
            Self::Fn => 6,
            Self::Unknown | Self::Generic => 7,
            Self::SelfRef => 7,
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

        // Dims 0-7: Signature shape
        v[0] = (self.signature.arity as f32).min(20.0) / 20.0;
        let total_params = self.signature.param_categories.len().max(1) as f32;
        for cat in &self.signature.param_categories {
            v[1 + cat.dim_index()] += 1.0 / total_params;
        }

        // Dims 8-15: Return type
        if let Some(ret) = &self.signature.return_category {
            v[8 + ret.dim_index()] = 1.0;
        }
        v[14] = self.signature.return_nesting as f32 / 3.0;
        v[15] = if self.signature.return_wraps_result { 0.5 } else { 0.0 }
            + if self.signature.return_wraps_option { 0.5 } else { 0.0 };

        // Dims 16-23: Self/receiver
        v[16] = if self.signature.has_self { 1.0 } else { 0.0 };
        v[17] = if self.signature.is_mut_self { 1.0 } else { 0.0 };
        v[18] = if self.signature.is_static { 1.0 } else { 0.0 };
        v[19] = if self.signature.is_method { 1.0 } else { 0.0 };
        v[20] = if self.signature.is_constructor { 1.0 } else { 0.0 };

        // Dims 24-39: First 4 params positionally (4 dims each)
        for (i, cat) in self.signature.param_categories.iter().take(4).enumerate() {
            let base = 24 + i * 4;
            let idx = cat.dim_index().min(3);
            v[base + idx] = 1.0;
        }

        // Dims 40-47: Global shape (log1p normalized)
        let cf = &self.control_flow;
        let sc = &self.semantic_counts;
        let total_nodes = cf.branches + cf.loops + cf.early_returns
            + cf.error_propagations + cf.unsafe_blocks + cf.match_arms
            + cf.closures + cf.awaits + sc.calls + sc.assignments
            + sc.member_access + sc.index_ops + sc.binary_ops
            + sc.collection_literals + sc.type_annotations + sc.lambdas;
        v[40] = (total_nodes as f32).ln_1p() / 10.0;

        // Dims 48-55: Control-flow sketch (log1p scaled to [0,1])
        let cf_vals = [
            cf.branches, cf.loops, cf.early_returns, cf.error_propagations,
            cf.unsafe_blocks, cf.match_arms, cf.closures, cf.awaits,
        ];
        let cf_max = cf_vals.iter().copied().max().unwrap_or(1).max(1) as f32;
        for (i, &val) in cf_vals.iter().enumerate() {
            v[48 + i] = (val as f32).ln_1p() / cf_max.ln_1p().max(f32::EPSILON);
        }

        // Dims 56-63: Semantic counts (log1p scaled to [0,1])
        let sc_vals = [
            sc.calls, sc.assignments, sc.member_access, sc.index_ops,
            sc.binary_ops, sc.collection_literals, sc.type_annotations, sc.lambdas,
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
    if denom < f32::EPSILON { 0.0 } else { dot / denom }
}
```

- [ ] **Step 4: Register module**

Add to `src/context/extract/mod.rs`:
```rust
pub mod fingerprint;

#[cfg(test)]
mod fingerprint_tests;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum fingerprint_tests -- --nocapture`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add src/context/extract/fingerprint.rs src/context/extract/fingerprint_tests.rs src/context/extract/mod.rs
git commit -m "feat: add structural fingerprint core types and to_vector encoding"
```

---

### Task 2: Rust Fingerprinter

**Files:**
- Create: `src/context/extract/fingerprint_rust.rs`
- Test: `src/context/extract/fingerprint_rust_tests.rs`
- Modify: `src/context/extract/mod.rs` — add module declarations

- [ ] **Step 1: Write failing tests**

```rust
// src/context/extract/fingerprint_rust_tests.rs
use super::fingerprint::*;
use super::fingerprint_rust::RustFingerprinter;

#[test]
fn fingerprints_simple_function() {
    let src = r#"
fn add(a: u32, b: u32) -> u32 {
    let result = a + b;
    if result > 100 {
        return 0;
    }
    result
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    assert!(fp.is_some(), "should produce fingerprint");
    let fp = fp.unwrap();
    assert_eq!(fp.signature.arity, 2);
    assert!(!fp.signature.has_self);
    assert_eq!(fp.signature.param_categories.len(), 2);
    assert_eq!(fp.signature.param_categories[0], TypeCategory::Prim);
    assert!(fp.control_flow.branches >= 1);
    assert!(fp.control_flow.early_returns >= 1);
}

#[test]
fn fingerprints_method_with_self() {
    let src = r#"
impl Foo {
    fn process(&self, items: Vec<String>, limit: usize) -> Result<Vec<Item>, Error> {
        let mut out = Vec::new();
        for item in &items {
            if item.len() > limit {
                out.push(Item::from(item));
            }
        }
        Ok(out)
    }
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    assert!(fp.is_some());
    let fp = fp.unwrap();
    assert!(fp.signature.has_self);
    assert!(fp.signature.is_method);
    assert_eq!(fp.signature.param_categories.len(), 2); // excludes self
    assert_eq!(fp.signature.param_categories[0], TypeCategory::Col);
    assert_eq!(fp.signature.param_categories[1], TypeCategory::Prim);
    assert!(fp.signature.return_wraps_result);
    assert!(fp.control_flow.loops >= 1);
    assert!(fp.control_flow.branches >= 1);
}

#[test]
fn skips_trivial_function() {
    let src = r#"
fn name(&self) -> &str {
    &self.name
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    assert!(fp.is_none(), "trivial getter should be skipped");
}

#[test]
fn counts_error_propagation() {
    let src = r#"
fn load(path: &str) -> Result<Data, Error> {
    let file = std::fs::read_to_string(path)?;
    let parsed = serde_json::from_str(&file)?;
    let validated = validate(parsed)?;
    if validated.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Data::new(validated))
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    assert!(fp.is_some());
    let fp = fp.unwrap();
    assert!(fp.control_flow.error_propagations >= 3);
}

#[test]
fn vector_is_64_dims_and_finite() {
    let src = r#"
fn complex(a: Vec<u32>, b: HashMap<String, Vec<u8>>, c: Option<bool>) -> Result<String, Error> {
    let mut result = String::new();
    for item in &a {
        if let Some(val) = b.get(&item.to_string()) {
            for byte in val {
                result.push(*byte as char);
            }
        }
    }
    match c {
        Some(true) => Ok(result),
        Some(false) => Ok(String::new()),
        None => Err(Error::Missing),
    }
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src).unwrap();
    let vec = fp.to_vector();
    assert_eq!(vec.len(), 64);
    assert!(vec.iter().all(|v| v.is_finite()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --bin quorum fingerprint_rust_tests -- --nocapture 2>&1 | head -20`
Expected: FAIL — module not found

- [ ] **Step 3: Implement RustFingerprinter**

Create `src/context/extract/fingerprint_rust.rs` with `RustFingerprinter` struct that:
- Uses `ast_grep_core::language::SupportLang::Rust` to parse source
- Walks the tree-sitter AST to find function/method declarations
- Classifies parameter types via `TypeCategory::classify_rust`
- Counts control-flow nodes (if, for, while, loop, return, ?, unsafe, match arms, closures, .await)
- Counts semantic nodes (calls, assignments, field access, index, binary ops, vec!/array literals, type annotations, closures)
- Returns `None` when body node count < `MIN_BODY_NODE_COUNT`
- Provides `fingerprint_source(&self, src: &str) -> Option<StructuralFingerprint>` convenience method for testing
- Provides `fingerprint_node(&self, node: &tree_sitter::Node, source: &[u8]) -> Option<StructuralFingerprint>` for production use

- [ ] **Step 4: Register module in mod.rs**

Add to `src/context/extract/mod.rs`:
```rust
pub mod fingerprint_rust;

#[cfg(test)]
mod fingerprint_rust_tests;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --bin quorum fingerprint_rust_tests -- --nocapture`
Expected: All PASS

- [ ] **Step 6: Commit**

```bash
git add src/context/extract/fingerprint_rust.rs src/context/extract/fingerprint_rust_tests.rs src/context/extract/mod.rs
git commit -m "feat: add Rust AST fingerprinter"
```

---

### Task 3: Python Fingerprinter

**Files:**
- Create: `src/context/extract/fingerprint_python.rs`
- Test: `src/context/extract/fingerprint_python_tests.rs`
- Modify: `src/context/extract/mod.rs`

Same pattern as Task 2 but for Python. Key differences:
- Uses `SupportLang::Python` to parse
- `self`/`cls` first param is detected and excluded from param_categories
- Untyped params get `TypeCategory::Unknown` (not Generic)
- Type annotations from `: type` syntax are parsed when present
- `try/except` counts as error_propagation
- `raise` counts as early_return
- `with` statement counts as a branch
- List/dict/set comprehensions count as both loops and collection_literals

Tests should cover: simple function, method with self, untyped params produce Unknown, type-annotated params classify correctly, comprehension counting, try/except counting, trivial function skipped.

- [ ] **Step 1-6:** Same TDD cycle as Task 2

---

### Task 4: TypeScript Fingerprinter

**Files:**
- Create: `src/context/extract/fingerprint_typescript.rs`
- Test: `src/context/extract/fingerprint_typescript_tests.rs`
- Modify: `src/context/extract/mod.rs`

Same pattern as Task 2 but for TypeScript. Key differences:
- Uses `SupportLang::TypeScript` to parse
- `this` param is detected and excluded
- `Promise<T>` maps to `Res` category
- `T | undefined` and `T | null` map to `Opt`
- Arrow functions and function declarations both fingerprinted
- `try/catch` counts as error_propagation
- `throw` counts as early_return
- `await` expressions counted
- `?.` optional chaining counts as a branch

Tests should cover: function declaration, arrow function, class method with this, Promise return type, union types, async/await counting, optional chaining, trivial function skipped.

- [ ] **Step 1-6:** Same TDD cycle as Task 2

---

### Task 5: Schema Migration (v1 -> v2)

**Files:**
- Modify: `src/context/index/builder.rs` — add `chunks_struct_vec` table creation, fingerprint_version storage, schema migration
- Test: `src/context/index/builder_tests.rs` (or inline tests)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn creates_chunks_struct_vec_table() {
    let dir = tempdir().unwrap();
    let emb = HashEmbedder;
    let clock = FixedClock::epoch();
    let builder = IndexBuilder::new(dir.path().join("test.db").as_ref(), &clock, &emb).unwrap();
    // Table should exist
    let count: i64 = builder.conn().query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chunks_struct_vec'",
        [],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn stores_fingerprint_version_in_state() {
    let dir = tempdir().unwrap();
    let emb = HashEmbedder;
    let clock = FixedClock::epoch();
    let builder = IndexBuilder::new(dir.path().join("test.db").as_ref(), &clock, &emb).unwrap();
    let version: String = builder.conn().query_row(
        "SELECT value FROM state WHERE key = 'fingerprint_version'",
        [],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(version, "structural-v1");
}

#[test]
fn requires_refingerprinting_on_version_mismatch() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    {
        let emb = HashEmbedder;
        let clock = FixedClock::epoch();
        let builder = IndexBuilder::new(&db_path, &clock, &emb).unwrap();
        builder.conn().execute(
            "UPDATE state SET value = 'old-version' WHERE key = 'fingerprint_version'",
            [],
        ).unwrap();
    }
    let emb = HashEmbedder;
    let clock = FixedClock::epoch();
    let builder = IndexBuilder::new(&db_path, &clock, &emb).unwrap();
    assert!(builder.requires_refingerprinting().unwrap());
}

#[test]
fn migrates_v1_db_adds_struct_vec_table() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    // Create a v1 DB manually (without chunks_struct_vec)
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY, value TEXT);").unwrap();
        conn.execute("INSERT INTO state VALUES ('schema_version', '1')", []).unwrap();
    }
    // Opening with IndexBuilder should migrate
    let emb = HashEmbedder;
    let clock = FixedClock::epoch();
    let builder = IndexBuilder::new(&db_path, &clock, &emb).unwrap();
    let count: i64 = builder.conn().query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='chunks_struct_vec'",
        [],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 1);
}
```

- [ ] **Step 2-5:** Standard TDD cycle

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: schema v2 migration with chunks_struct_vec table"
```

---

### Task 6: Index Builder — Insert Fingerprints

**Files:**
- Modify: `src/context/index/builder.rs` — add `insert_structural_fingerprint` method, integrate into `add_chunk`
- Modify: `src/context/extract/dispatch.rs` — compute fingerprints during extraction

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn insert_chunk_with_fingerprint_stores_in_struct_vec() {
    let dir = tempdir().unwrap();
    let emb = HashEmbedder;
    let clock = FixedClock::epoch();
    let mut builder = IndexBuilder::new(dir.path().join("test.db").as_ref(), &clock, &emb).unwrap();
    let chunk = /* create test chunk */;
    let fp_vec = [0.5f32; 64];
    builder.add_chunk_with_fingerprint(&chunk, Some(&fp_vec)).unwrap();

    let count: i64 = builder.conn().query_row(
        "SELECT COUNT(*) FROM chunks_struct_vec WHERE id = ?1",
        params![chunk.id],
        |r| r.get(0),
    ).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn insert_chunk_without_fingerprint_skips_struct_vec() {
    // Pass None for fingerprint, verify no row in chunks_struct_vec
}
```

- [ ] **Step 2-5:** Standard TDD cycle

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: insert structural fingerprints into chunks_struct_vec during indexing"
```

---

### Task 7: Retrieval Leg — Structural Fingerprint KNN

**Files:**
- Modify: `src/context/retrieve/retriever.rs` — add `RetrievalLeg::StructuralFingerprint`, add `structural_fingerprints` field to `RetrievalQuery`, add KNN query method
- Test: `src/context/retrieve/retriever_tests.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn structural_fingerprint_leg_returns_similar_chunks() {
    // Index chunks with known fingerprints
    // Query with a similar fingerprint
    // Verify matches returned with StructuralFingerprint leg
}

#[test]
fn structural_fingerprint_leg_skipped_when_no_fingerprints() {
    // Query with empty structural_fingerprints
    // Verify results identical to baseline (only BM25+Vector+Structural)
}

#[test]
fn structural_fingerprint_leg_skipped_when_table_missing() {
    // Use a v1 DB without chunks_struct_vec
    // Verify no error, results from other legs only
}
```

- [ ] **Step 2-5:** Standard TDD cycle

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: add structural fingerprint KNN retrieval leg"
```

---

### Task 8: Reranker — Additive Structural Boost

**Files:**
- Modify: `src/context/retrieve/rerank.rs` — add `struct_sim` to `ScoreBreakdown`, apply additive boost
- Test: `src/context/retrieve/rerank_tests.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn struct_sim_boost_is_additive() {
    // Two identical chunks, one with struct_sim=0.9, one with struct_sim=0.0
    // Verify boosted chunk scores higher by exactly STRUCT_BOOST_WEIGHT * 0.9
    // Verify BM25/vector blend weights unchanged (still 0.6/0.4)
}

#[test]
fn struct_sim_zero_produces_baseline_score() {
    // Chunk with struct_sim=0.0
    // Score should equal (0.6*bm25 + 0.4*vec + id_boost + path_boost) * recency
}

#[test]
fn ablation_zero_weight_disables_structural_signal() {
    // With STRUCT_BOOST_WEIGHT=0.0, struct_sim=0.9 chunk has same score as struct_sim=0.0
}
```

- [ ] **Step 2-5:** Standard TDD cycle

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: add structural similarity as additive reranker boost"
```

---

### Task 9: Bootstrap — Query-Side Fingerprinting

**Files:**
- Modify: `src/context/bootstrap.rs` — parse file-under-review, fingerprint symbols, populate `RetrievalQuery.structural_fingerprints`
- Test: `src/context/bootstrap.rs` (inline tests) or integration test

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn build_query_fingerprints_from_rust_source() {
    let src = r#"
fn process(items: Vec<String>) -> Result<Vec<Item>, Error> {
    let mut out = Vec::new();
    for item in items {
        let parsed = parse(&item)?;
        if parsed.is_valid() {
            out.push(parsed.into());
        }
    }
    Ok(out)
}

fn name(&self) -> &str { &self.name }
"#;
    let fps = build_query_fingerprints(src, "rust");
    // Should fingerprint process() but skip trivial name()
    assert_eq!(fps.len(), 1);
    assert_eq!(fps[0].1, "process"); // qualified_name
    assert_eq!(fps[0].0.len(), 64);
}

#[test]
fn caps_at_max_query_symbols() {
    // Source with 20 functions, verify only MAX_QUERY_SYMBOLS returned
    // Selected by body size descending
}
```

- [ ] **Step 2-5:** Standard TDD cycle

- [ ] **Step 6: Commit**

```bash
git commit -m "feat: query-side fingerprinting for file under review"
```

---

### Task 10: Telemetry Fields

**Files:**
- Modify: `src/review_log.rs` — add three new fields to `ContextTelemetry`
- Modify: `src/dimensions.rs` — update test helper

- [ ] **Step 1: Add fields**

```rust
// In ContextTelemetry:
#[serde(default)]
pub structural_fingerprint_hits: u32,
#[serde(default)]
pub structural_fingerprint_contributed: u32,
#[serde(default)]
pub fingerprint_query_ms: u32,
```

- [ ] **Step 2: Update test helpers and verify compilation**

Run: `cargo test --bin quorum`
Expected: All PASS (fields default to 0 via serde)

- [ ] **Step 3: Commit**

```bash
git commit -m "feat: add structural fingerprint telemetry fields"
```

---

### Task 11: Integration Test — End-to-End

**Files:**
- Create or modify: `src/context/phase7_integration_tests.rs` (or similar)

- [ ] **Step 1: Write integration test**

Test the full pipeline: create 2 source indexes with fingerprinted chunks, build a retriever, query with structural fingerprints, verify fingerprint-leg chunks appear in results alongside BM25/vector results.

- [ ] **Step 2: Write ablation integration test**

Same setup but with `STRUCT_BOOST_WEIGHT = 0.0`. Verify fingerprint-leg chunks still appear in telemetry but don't affect ranking.

- [ ] **Step 3: Write golden test**

Parse a canonical Rust snippet, verify the fingerprint vector matches a known expected value. This catches regressions in the fingerprint encoding.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --bin quorum`
Expected: All PASS

- [ ] **Step 5: Commit**

```bash
git commit -m "test: add structural fingerprint integration and golden tests"
```
