// Fixture: nullish-coalescing-broad
declare const count: number | null;
declare const label: string | null;
declare const flag: boolean | null;

// match: || where ?? may be more appropriate
const displayCount = count || 0;  // ruleid: nullish-coalescing-broad

// match: || with string default
const displayLabel = label || "default";  // ruleid: nullish-coalescing-broad

// match: || with boolean default
const isEnabled = flag || false;  // ruleid: nullish-coalescing-broad

// no-match: already using ??
const safeCount = count ?? 0;  // ok: nullish-coalescing-broad

// no-match: ?? with string
const safeLabel = label ?? "default";  // ok: nullish-coalescing-broad
