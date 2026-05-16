use super::fingerprint::FINGERPRINT_DIMS;
use super::fingerprint_rust::RustFingerprinter;

#[test]
fn fingerprints_simple_function() {
    let src = r#"
fn add(a: u32, b: u32) -> u32 {
    if a > b {
        return a;
    }
    let sum = a + b;
    let diff = a - b;
    let product = a * b;
    sum + diff + product
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for non-trivial function");

    assert_eq!(fp.signature.arity, 2, "two params (a, b)");
    assert!(!fp.signature.has_self, "free function, no self");
    assert!(!fp.signature.is_method, "not inside impl");
    assert!(fp.control_flow.branches >= 1, "should count the if branch");
    assert!(
        fp.control_flow.early_returns >= 1,
        "should count the return"
    );
}

#[test]
fn fingerprints_method_with_self() {
    let src = r#"
struct Foo;
impl Foo {
    fn process(&self, items: Vec<String>) -> Result<Vec<Item>, Error> {
        let mut result = Vec::new();
        for item in &items {
            if item.is_empty() {
                continue;
            }
            let parsed = parse(item);
            let validated = validate(&parsed);
            result.push(validated);
        }
        Ok(result)
    }
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for method");

    assert!(fp.signature.has_self, "method has &self");
    assert!(!fp.signature.is_mut_self, "immutable self");
    assert!(fp.signature.is_method, "inside impl block");
    assert!(!fp.signature.is_static, "has self, so not static");
    assert_eq!(fp.signature.arity, 1, "one param besides self");
    assert!(fp.signature.return_wraps_result, "return type wraps Result");
    assert!(fp.control_flow.loops >= 1, "has a for loop");
    assert!(fp.control_flow.branches >= 1, "has an if branch");
}

#[test]
fn skips_trivial_function() {
    let src = r#"
struct Foo { name: String }
impl Foo {
    fn name(&self) -> &str {
        &self.name
    }
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    assert!(
        fp.is_none(),
        "trivial getter should return None (body too small)"
    );
}

#[test]
fn counts_error_propagation() {
    let src = r#"
fn load_config(path: &str) -> Result<Config, Error> {
    let raw = std::fs::read_to_string(path)?;
    let parsed = parse_toml(&raw)?;
    let validated = validate_config(&parsed)?;
    let normalized = normalize(&validated);
    let checked = check(&normalized);
    let final_config = finalize(checked);
    Ok(final_config)
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert!(
        fp.control_flow.error_propagations >= 3,
        "should count at least 3 ? operators, got {}",
        fp.control_flow.error_propagations
    );
}

#[test]
fn vector_is_64_dims_and_finite() {
    let src = r#"
fn complex(a: u32, b: String, c: Vec<u8>) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for i in 0..a {
        if i % 2 == 0 {
            let s = format!("{}: {}", b, i);
            out.push(s);
        } else {
            let val = c.get(i as usize);
            if let Some(v) = val {
                out.push(v.to_string());
            }
        }
    }
    let filtered = out.iter().filter(|s| !s.is_empty()).cloned().collect();
    Ok(filtered)
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("complex function should fingerprint");

    let vec = fp.to_vector();
    assert_eq!(vec.len(), FINGERPRINT_DIMS);
    assert!(
        vec.iter().all(|v| v.is_finite()),
        "all vector dimensions must be finite"
    );
    // At least some dimensions should be nonzero for a nontrivial function.
    assert!(
        vec.iter().any(|v| *v > 0.0),
        "vector should have nonzero entries"
    );
}

#[test]
fn fingerprints_static_method() {
    let src = r#"
struct Foo { x: u32 }
impl Foo {
    fn new(x: u32) -> Self {
        let validated = validate(x);
        let clamped = clamp(validated, 0, 100);
        let normalized = normalize(clamped);
        let result = process(normalized);
        let final_val = finalize(result);
        Self { x: final_val }
    }
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("static method should fingerprint");

    assert!(!fp.signature.has_self, "no self param");
    assert!(fp.signature.is_method, "inside impl");
    assert!(fp.signature.is_static, "no self => static");
    assert!(fp.signature.is_constructor, "named 'new', returns Self");
}

#[test]
fn counts_match_arms() {
    let src = r#"
fn classify(val: u32) -> &'static str {
    let extra = compute(val);
    let adjusted = adjust(extra);
    let label = match adjusted {
        0 => "zero",
        1..=9 => "small",
        10..=99 => "medium",
        _ => "large",
    };
    label
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("match function should fingerprint");

    assert!(
        fp.control_flow.match_arms >= 4,
        "should count at least 4 match arms, got {}",
        fp.control_flow.match_arms
    );
}

#[test]
fn counts_unsafe_blocks() {
    let src = r#"
fn dangerous(ptr: *const u8, len: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(len);
    let slice = unsafe {
        std::slice::from_raw_parts(ptr, len)
    };
    for &byte in slice {
        let transformed = transform(byte);
        let validated = check(transformed);
        result.push(validated);
    }
    result
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("unsafe function should fingerprint");

    assert!(
        fp.control_flow.unsafe_blocks >= 1,
        "should count at least 1 unsafe block, got {}",
        fp.control_flow.unsafe_blocks
    );
}

#[test]
fn counts_closures() {
    let src = r#"
fn transform(items: Vec<u32>) -> Vec<String> {
    let filtered = items.iter()
        .filter(|x| **x > 10)
        .map(|x| x.to_string())
        .collect::<Vec<_>>();
    let mapped = filtered.iter()
        .map(|s| format!("item: {}", s))
        .collect();
    mapped
}
"#;
    let fp = RustFingerprinter.fingerprint_source(src);
    let fp = fp.expect("closure-heavy function should fingerprint");

    assert!(
        fp.control_flow.closures >= 2,
        "should count at least 2 closures, got {}",
        fp.control_flow.closures
    );
    assert!(
        fp.semantic_counts.lambdas >= 2,
        "semantic lambdas should count closures too, got {}",
        fp.semantic_counts.lambdas
    );
}
