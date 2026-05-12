import { Pool } from "pg";

// --- TRUE POSITIVE: string-format-sql ---
// User input directly interpolated into SQL query.
export async function getUserByName(pool: Pool, name: string) {
  const query = `SELECT * FROM users WHERE name = '${name}'`;  // TP: SQL injection
  return pool.query(query);
}

// --- TRUE POSITIVE: string-format-sql ---
// Template literal builds a DELETE statement with unescaped input.
export async function deleteOldRecords(pool: Pool, table: string, days: number) {
  const query = `DELETE FROM ${table} WHERE created_at < NOW() - INTERVAL '${days} days'`;  // TP: table name injection
  return pool.query(query);
}

// --- FALSE POSITIVE: string-format-sql ---
// SQL keyword appears in a user-facing message, not an actual query.
export function formatError(operation: string): string {
  return `Failed to SELECT data during ${operation}. Please retry.`;  // FP: not a real SQL query
}

// --- FALSE POSITIVE: string-format-sql ---
// SQL keyword in a logging/documentation template string.
export function logMigration(version: string): string {
  return `Migration ${version}: will UPDATE schema and INSERT seed data`;  // FP: documentation text
}

// --- TRUE POSITIVE: nullish-coalescing-broad ---
// || treats 0 as falsy, falling through to the default when 0 is valid.
export function getPort(config: { port?: number }): number {
  return config.port || 3000;  // TP: port=0 would fall through to 3000
}

// --- TRUE POSITIVE: nullish-coalescing-broad ---
// || treats "" as falsy; empty string is a valid username override.
export function getDisplayName(user: { name?: string }): string {
  return user.name || "Anonymous";  // TP: empty string "" falls through
}

// --- FALSE POSITIVE: nullish-coalescing-broad ---
// || is correct here: both null/undefined AND empty string should use default.
export function getRequiredLabel(label?: string): string {
  return label || "Untitled";  // FP: empty string SHOULD fall through to default
}

// --- FALSE POSITIVE: nullish-coalescing-broad ---
// || on booleans is a standard pattern for default-true behavior.
export function isEnabled(flag?: boolean): boolean {
  return flag || false;  // FP: boolean || boolean is idiomatic
}

// --- Non-speculative code for context ---
export interface DatabaseConfig {
  host: string;
  port: number;
  database: string;
  ssl: boolean;
}

export function buildConnectionString(config: DatabaseConfig): string {
  const proto = config.ssl ? "postgresql+ssl" : "postgresql";
  return `${proto}://${config.host}:${config.port}/${config.database}`;
}

export async function healthCheck(pool: Pool): Promise<boolean> {
  const result = await pool.query("SELECT 1 as healthy");
  return result.rows[0]?.healthy === 1;
}

// Proper parameterized query — not flagged.
export async function getUserById(pool: Pool, id: number) {
  return pool.query("SELECT * FROM users WHERE id = $1", [id]);
}
