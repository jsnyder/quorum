// TP: should match - non-null assertion bypasses type safety
const x = foo!.bar;  // ruleid: non-null-assertion

const y = array[0]!.toString();  // ruleid: non-null-assertion

// FP: should NOT match - normal property access
const z = foo.bar;  // ok: non-null-assertion

// FP: should NOT match - optional chaining
const w = foo?.bar;  // ok: non-null-assertion
