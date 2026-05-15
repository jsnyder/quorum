use super::fingerprint::FINGERPRINT_DIMS;
use super::fingerprint_python::PythonFingerprinter;

#[test]
fn fingerprints_simple_function() {
    let src = r#"
def calculate(a, b):
    if a > b:
        return a
    total = a + b
    diff = a - b
    product = a * b
    adjusted = total + diff
    result = adjusted + product
    return result
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for non-trivial function");

    assert_eq!(fp.signature.arity, 2, "two params (a, b)");
    assert!(!fp.signature.has_self, "free function, no self");
    assert!(!fp.signature.is_method, "not inside class");
    assert!(fp.control_flow.branches >= 1, "should count the if branch");
    assert!(
        fp.control_flow.early_returns >= 1,
        "should count the return"
    );
}

#[test]
fn fingerprints_method_with_self() {
    let src = r#"
class Processor:
    def process(self, items: list, limit: int) -> list:
        result = []
        for item in items:
            if item is None:
                continue
            parsed = self.parse(item)
            validated = self.validate(parsed)
            result.append(validated)
            if len(result) >= limit:
                break
        return result
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for method");

    assert!(fp.signature.has_self, "method has self");
    assert!(fp.signature.is_method, "inside class");
    assert!(!fp.signature.is_static, "has self, so not static");
    assert_eq!(fp.signature.arity, 2, "two params besides self");
    assert!(fp.control_flow.loops >= 1, "has a for loop");
    assert!(fp.control_flow.branches >= 1, "has an if branch");
}

#[test]
fn skips_trivial_function() {
    let src = r#"
class Foo:
    def name(self):
        return self.name
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    assert!(
        fp.is_none(),
        "trivial getter should return None (body too small)"
    );
}

#[test]
fn untyped_params_produce_unknown() {
    let src = r#"
def foo(a, b):
    x = a + b
    y = x * 2
    z = y - 1
    w = z + x
    q = w * y
    result = process(q)
    more = transform(result)
    final_val = cleanup(more)
    return final_val
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert_eq!(fp.signature.arity, 2, "two params");
    for (i, cat) in fp.signature.param_categories.iter().enumerate() {
        assert_eq!(
            *cat,
            super::fingerprint::TypeCategory::Unknown,
            "param {i} should be Unknown (no type annotation)"
        );
    }
}

#[test]
fn typed_params_classified_correctly() {
    let src = r#"
def bar(x: int, y: str) -> list:
    a = x + 1
    b = y.upper()
    c = str(a) + b
    items = [c]
    items.append(b)
    more = items * x
    result = sorted(more)
    filtered = [i for i in result if i]
    return filtered
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert_eq!(fp.signature.arity, 2, "two params");
    assert_eq!(
        fp.signature.param_categories[0],
        super::fingerprint::TypeCategory::Prim,
        "int should classify as Prim"
    );
    assert_eq!(
        fp.signature.param_categories[1],
        super::fingerprint::TypeCategory::Str,
        "str should classify as Str"
    );
    assert_eq!(
        fp.signature.return_category,
        Some(super::fingerprint::TypeCategory::Col),
        "list return should classify as Col"
    );
}

#[test]
fn counts_try_except() {
    let src = r#"
def load_data(path):
    try:
        raw = read_file(path)
        parsed = parse_json(raw)
        validated = validate_schema(parsed)
        normalized = normalize(validated)
        return normalized
    except FileNotFoundError:
        log_error("not found")
        return None
    except ValueError:
        log_error("bad value")
        return None
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert!(
        fp.control_flow.error_propagations >= 1,
        "should count try statement as error_propagation, got {}",
        fp.control_flow.error_propagations
    );
    // try_statement counts as 1, each except_clause counts as 1
    assert!(
        fp.control_flow.error_propagations >= 3,
        "should count try + 2 except clauses, got {}",
        fp.control_flow.error_propagations
    );
}

#[test]
fn counts_comprehensions() {
    let src = r#"
def transform(data):
    squares = [x * x for x in data]
    lookup = {k: v for k, v in enumerate(data)}
    uniques = {x for x in data if x > 0}
    combined = squares + list(lookup.values())
    filtered = [x for x in combined if x > 10]
    result = sorted(filtered)
    return result
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    // Comprehensions count as both loops and collection_literals
    assert!(
        fp.control_flow.loops >= 3,
        "should count at least 3 comprehension-loops, got {}",
        fp.control_flow.loops
    );
    assert!(
        fp.semantic_counts.collection_literals >= 3,
        "should count at least 3 collection literals from comprehensions, got {}",
        fp.semantic_counts.collection_literals
    );
}

#[test]
fn vector_is_64_dims_and_finite() {
    let src = r#"
def complex_func(a: int, b: str, c: list) -> dict:
    out = {}
    for i in range(a):
        if i % 2 == 0:
            s = f"{b}: {i}"
            out[s] = c[i] if i < len(c) else None
        else:
            val = c[i] if i < len(c) else ""
            if val:
                out[val] = i
    filtered = {k: v for k, v in out.items() if v is not None}
    result = dict(sorted(filtered.items()))
    return result
"#;
    let fp = PythonFingerprinter.fingerprint_source(src);
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
