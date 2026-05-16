use super::fingerprint::FINGERPRINT_DIMS;
use super::fingerprint_typescript::TypeScriptFingerprinter;

/// Regression test: a parent function containing a nested arrow function should
/// NOT have its control-flow or semantic counts inflated by the inner function's
/// body.  The DFS must stop at nested function boundaries.
#[test]
fn nested_arrow_does_not_inflate_parent_counts() {
    let src = r#"
function outer(items: string[]): () => void {
    const threshold = 10;
    const label = "test";
    const tag = label + threshold;
    const debug = tag.length;
    const inner = () => {
        if (items.length > 0) {
            for (const item of items) {
                console.log(item);
            }
            return items[0];
        }
        throw new Error("empty");
    };
    const extra = debug + 1;
    return inner;
}
"#;

    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("outer function should fingerprint");

    // `outer` itself has NO if/for/while -- those live inside `inner`.
    assert_eq!(
        fp.control_flow.branches, 0,
        "outer has no branches; the if is inside inner"
    );
    assert_eq!(
        fp.control_flow.loops, 0,
        "outer has no loops; the for-of is inside inner"
    );

    // `outer` has one early return: `return inner;`
    assert_eq!(
        fp.control_flow.early_returns, 1,
        "outer has one return statement (return inner)"
    );

    // `outer` defines one closure (the arrow function `inner`).
    assert_eq!(
        fp.control_flow.closures, 1,
        "outer defines exactly one nested arrow function"
    );

    // Semantic counts: the `throw` inside `inner` should NOT leak out.
    // `outer` should count `inner` as exactly 1 lambda.
    assert_eq!(
        fp.semantic_counts.lambdas, 1,
        "outer has exactly one lambda (inner)"
    );

    // The call_expression count should only reflect calls in outer's scope
    // (not console.log inside inner).  Outer has zero explicit call_expression
    // nodes in its own scope (the arrow is an assignment, not a call).
    assert_eq!(
        fp.semantic_counts.calls, 0,
        "outer has no call expressions in its own scope"
    );
}

/// The inner arrow function, when fingerprinted on its own, should carry
/// its own control-flow counts.
#[test]
fn inner_arrow_carries_own_counts() {
    let src = r#"
function outer(items: string[]): () => void {
    const threshold = 10;
    const label = "test";
    const tag = label + threshold;
    const debug = tag.length;
    const inner = () => {
        if (items.length > 0) {
            for (const item of items) {
                console.log(item);
            }
            return items[0];
        }
        throw new Error("empty");
    };
    const extra = debug + 1;
    return inner;
}
"#;

    let fprinter = TypeScriptFingerprinter;
    let results = fprinter.fingerprint_all_functions(src);
    // Should have entries for "outer" and "inner" (if inner is large enough).
    let inner_fp = results.iter().find(|(name, _)| name == "inner");
    // inner might be too small for MIN_BODY_NODE_COUNT; if it fingerprints,
    // check that its counts are self-contained.
    if let Some((_, fp)) = inner_fp {
        assert!(
            fp.control_flow.branches >= 1,
            "inner has an if branch"
        );
        assert!(
            fp.control_flow.loops >= 1,
            "inner has a for-of loop"
        );
    }
}

#[test]
fn fingerprints_simple_function() {
    let src = r#"
function calculate(a: number, b: number): number {
    if (a > b) {
        return a;
    }
    const total = a + b;
    const diff = a - b;
    const product = a * b;
    const adjusted = total + diff;
    const result = adjusted + product;
    return result;
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for non-trivial function");

    assert_eq!(fp.signature.arity, 2, "two params (a, b)");
    assert!(!fp.signature.has_self, "free function, no this");
    assert!(!fp.signature.is_method, "not inside class");
    assert!(fp.control_flow.branches >= 1, "should count the if branch");
    assert!(
        fp.control_flow.early_returns >= 1,
        "should count the return"
    );
}

#[test]
fn fingerprints_method_with_this() {
    let src = r#"
class Processor {
    process(this: Processor, items: string[], limit: number): string[] {
        const result: string[] = [];
        for (const item of items) {
            if (item === null) {
                continue;
            }
            const parsed = this.parse(item);
            const validated = this.validate(parsed);
            result.push(validated);
            if (result.length >= limit) {
                break;
            }
        }
        return result;
    }
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint for method with explicit this");

    assert!(fp.signature.has_self, "method has explicit this param");
    assert!(fp.signature.is_method, "inside class");
    assert!(!fp.signature.is_static, "has this, so not static");
    assert_eq!(fp.signature.arity, 2, "two params besides this");
    assert!(fp.control_flow.loops >= 1, "has a for-of loop");
    assert!(fp.control_flow.branches >= 1, "has an if branch");
}

#[test]
fn skips_trivial_function() {
    let src = r#"
function getName(): string {
    return x;
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    assert!(
        fp.is_none(),
        "trivial getter should return None (body too small)"
    );
}

#[test]
fn arrow_function_fingerprint() {
    let src = r#"
const transform = (items: number[]): number[] => {
    const filtered = items.filter(x => x > 0);
    const doubled = filtered.map(x => x * 2);
    const sorted = doubled.sort((a, b) => a - b);
    const capped = sorted.slice(0, 10);
    const result = capped.map(x => x + 1);
    const final_val = result.reduce((a, b) => a + b, 0);
    console.log(final_val);
    return result;
};
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("arrow function with body should fingerprint");

    assert_eq!(fp.signature.arity, 1, "one param (items)");
    assert!(!fp.signature.has_self, "arrow function, no this");
    // Nested arrow functions count as closures and lambdas.
    assert!(
        fp.control_flow.closures >= 1,
        "should count nested arrow functions as closures, got {}",
        fp.control_flow.closures
    );
    assert!(
        fp.semantic_counts.lambdas >= 1,
        "should count nested arrow functions as lambdas, got {}",
        fp.semantic_counts.lambdas
    );
}

#[test]
fn typed_params_classified() {
    let src = r#"
function bar(x: number, y: string, z: Array<number>): boolean {
    const a = x + 1;
    const b = y.toUpperCase();
    const c = z.length;
    const d = a * c;
    const e = b.length;
    const f = d + e;
    const g = f > 100;
    const h = g && c > 0;
    console.log(h);
    return h;
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert_eq!(fp.signature.arity, 3, "three params");
    assert_eq!(
        fp.signature.param_categories[0],
        super::fingerprint::TypeCategory::Prim,
        "number should classify as Prim"
    );
    assert_eq!(
        fp.signature.param_categories[1],
        super::fingerprint::TypeCategory::Str,
        "string should classify as Str"
    );
    assert_eq!(
        fp.signature.param_categories[2],
        super::fingerprint::TypeCategory::Col,
        "Array<number> should classify as Col"
    );
    assert_eq!(
        fp.signature.return_category,
        Some(super::fingerprint::TypeCategory::Prim),
        "boolean return should classify as Prim"
    );
}

#[test]
fn counts_try_catch() {
    let src = r#"
function loadData(path: string): object | null {
    try {
        const raw = readFile(path);
        const parsed = JSON.parse(raw);
        const validated = validateSchema(parsed);
        const normalized = normalize(validated);
        return normalized;
    } catch (error) {
        logError("failed to load");
        console.error(error);
        return null;
    }
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("should produce a fingerprint");

    assert!(
        fp.control_flow.error_propagations >= 1,
        "should count try statement as error_propagation, got {}",
        fp.control_flow.error_propagations
    );
    // try_statement counts as 1, catch_clause counts as 1
    assert!(
        fp.control_flow.error_propagations >= 2,
        "should count try + catch clause, got {}",
        fp.control_flow.error_propagations
    );
}

#[test]
fn vector_is_64_dims_and_finite() {
    let src = r#"
function complex(a: number, b: string, c: Array<number>): Map<string, number> {
    const out = new Map<string, number>();
    for (let i = 0; i < a; i++) {
        if (i % 2 === 0) {
            const s = `${b}: ${i}`;
            const val = c[i] !== undefined ? c[i] : 0;
            out.set(s, val);
        } else {
            const val = c[i] !== undefined ? c[i] : -1;
            if (val > 0) {
                out.set(String(val), i);
            }
        }
    }
    const filtered = new Map<string, number>();
    for (const [k, v] of out.entries()) {
        if (v > 0) {
            filtered.set(k, v);
        }
    }
    return filtered;
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
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
fn counts_await() {
    let src = r#"
async function fetchData(url: string): Promise<object> {
    const response = await fetch(url);
    const data = await response.json();
    const validated = validate(data);
    const transformed = transform(validated);
    const cached = await cacheResult(transformed);
    const logged = logResult(cached);
    const final_val = finalize(logged);
    return final_val;
}
"#;
    let fp = TypeScriptFingerprinter.fingerprint_source(src);
    let fp = fp.expect("async function should fingerprint");

    assert!(
        fp.control_flow.awaits >= 3,
        "should count at least 3 await expressions, got {}",
        fp.control_flow.awaits
    );
    assert_eq!(
        fp.signature.return_category,
        Some(super::fingerprint::TypeCategory::Res),
        "Promise return should classify as Res"
    );
    assert!(
        fp.signature.return_wraps_result,
        "should detect Promise wrapping"
    );
}

#[test]
fn static_constructor_detection() {
    let src = r#"
class Config {
    constructor(path: string) {
        const raw = readFile(path);
        const parsed = parseToml(raw);
        const validated = validateConfig(parsed);
        const normalized = normalizeConfig(validated);
        const checked = checkConfig(normalized);
        this.data = checked;
    }

    static create(path: string): Config {
        const config = new Config(path);
        const initialized = config.init();
        const validated = config.validate();
        const ready = config.prepare();
        const finalized = config.finalize();
        return finalized;
    }
}
"#;
    // Parse and find the constructor first.
    let fp_ctor = TypeScriptFingerprinter.fingerprint_source(src);
    let fp_ctor = fp_ctor.expect("constructor should fingerprint");

    assert!(fp_ctor.signature.is_constructor, "should detect constructor");
    assert!(fp_ctor.signature.is_method, "constructor is a method");
    assert!(fp_ctor.signature.has_self, "constructor has implicit this");

    // For the static method, we need to find it manually since
    // fingerprint_source returns the first function.
    let root = ast_grep_language::LanguageExt::ast_grep(&ast_grep_language::SupportLang::TypeScript, src);
    let root_node = root.root();
    let methods: Vec<_> = root_node
        .dfs()
        .filter(|n| n.kind().as_ref() == "method_definition")
        .collect();
    assert!(methods.len() >= 2, "should find at least 2 methods");

    let fp_static = TypeScriptFingerprinter
        .fingerprint_node(&methods[1], src)
        .expect("static method should fingerprint");

    assert!(fp_static.signature.is_static, "should detect static method");
    assert!(fp_static.signature.is_method, "static method is still a method");
    assert!(
        !fp_static.signature.has_self,
        "static method does not have this"
    );
}
