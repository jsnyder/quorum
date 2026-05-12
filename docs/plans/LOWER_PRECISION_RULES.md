# Candidate AST Rules (Lower Precision / Requires Judge)

The following rules were identified as high-value but may have lower precision or higher false-positive rates. They should be evaluated by a judge before promotion to bundled rules.

## 1. subprocess-no-check (Python)
- **Risk:** `subprocess.run()` without `check=True` ignores non-zero exit codes.
- **Precision Issue:** Some codebases use `returncode` manually; a naive AST rule might flag these valid cases.
- **Action:** Need to judge if manual check follows or if code is "fire and forget".

## 2. re-compile-in-loop (Python)
- **Risk:** Performance bottleneck when compiling the same regex inside a loop.
- **Precision Issue:** Hard to distinguish if the regex is truly static or depends on loop variables without deeper analysis.

## 3. string-byte-slice (Rust)
- **Risk:** `&s[..n]` can panic if `n` is not on a UTF-8 character boundary.
- **Precision Issue:** Many string slices are safe (e.g., ASCII-only or validated). High FP noise for non-localized strings.

## 4. jinja-loop-variable-scoping (YAML/Home Assistant)
- **Risk:** Accessing loop variables outside the loop scope (legacy behavior).
- **Precision Issue:** Regex-based detection in YAML templates is fragile and can miss context.

## 5. broad-exception-catch (Python)
- **Risk:** `except Exception:` catches everything.
- **Precision Issue:** Frequently used intentionally in top-level runners.

## 6. nullish-coalescing-preferred (TypeScript)
- **Risk:** Using `||` instead of `??` when `0`, `""`, or `false` are valid values.
- **Precision Issue:** Intent is often ambiguous; `||` is idiomatic when those falsy values should also trigger the default.
