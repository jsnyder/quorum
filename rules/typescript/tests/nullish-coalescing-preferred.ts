// TP: should match - || with literal default
const name = user.name || "Anonymous";  // ruleid: nullish-coalescing-preferred
const count = getCount() || 0;  // ruleid: nullish-coalescing-preferred
const items = data.items || [];  // ruleid: nullish-coalescing-preferred
const config = options.config || {};  // ruleid: nullish-coalescing-preferred

// FP: should NOT match - already using ??
const name2 = user.name ?? "Anonymous";  // ok: nullish-coalescing-preferred

// FP: should NOT match - boolean logic (not defaulting)
if (a || b) { doSomething(); }  // ok: nullish-coalescing-preferred

// FP: should NOT match - || with non-literal RHS
const x = a || b;  // ok: nullish-coalescing-preferred
