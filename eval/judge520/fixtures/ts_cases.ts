// Synthetic eval fixture for #520 part 2. NOT production code.

declare const db: { query(sql: string): Promise<unknown> };
declare const req: { params: Record<string, string>; query: Record<string, string> };

// ---- string-format-sql: TRUE POSITIVES --------------------------------------

/** TP: a path parameter is interpolated straight into the WHERE clause. */
export function getUser(): Promise<unknown> {
  return db.query(`SELECT * FROM users WHERE id = ${req.params.id}`);
}

/** TP: a query-string value reaches an ORDER BY, which cannot be parameterised
 *  and is not validated against an allowlist here. */
export function listOrders(): Promise<unknown> {
  return db.query(`SELECT id, total FROM orders ORDER BY ${req.query.sort} LIMIT 50`);
}

// ---- string-format-sql: TRUE NEGATIVES --------------------------------------

/** FP: a template literal with no interpolation at all. */
export function allUsers(): Promise<unknown> {
  return db.query(`SELECT id, email FROM users WHERE deleted_at IS NULL`);
}

/** FP: the only interpolated value is a compile-time constant defined above. */
const TABLE = "audit_log_2026";
export function auditCount(): Promise<unknown> {
  return db.query(`SELECT COUNT(*) FROM ${TABLE}`);
}

// ---- nullish-coalescing-broad: TRUE POSITIVES -------------------------------

/** TP: a configured retry budget of 0 ("never retry") is silently replaced
 *  by 3. Zero is a meaningful value for this setting. */
export function retryBudget(cfg: { retries?: number }): number {
  return cfg.retries || 3;
}

/** TP: an explicit `false` for a feature flag is overridden to the default
 *  `true`, so the flag cannot be turned off. */
export function featureOn(cfg: { enabled?: boolean }): boolean {
  return cfg.enabled || true;
}

// ---- nullish-coalescing-broad: TRUE NEGATIVES -------------------------------

/** FP: the empty string is not a meaningful display name; falling back is
 *  exactly the intent. */
export function displayName(u: { name?: string }): string {
  return u.name || "anonymous";
}

/** FP: a plain boolean disjunction, not a defaulting expression. */
export function canEdit(isOwner: boolean, isAdmin: boolean): boolean {
  return isOwner || isAdmin;
}

/** FP: defaulting an optional array; an empty array and undefined are treated
 *  identically downstream. */
export function tags(x: { tags?: string[] }): string[] {
  return x.tags || [];
}
