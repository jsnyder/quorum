// Fixture: string-format-sql
declare const db: { query: (sql: string) => any };
declare const userId: string;
declare const tableName: string;

// match: SELECT with interpolation in template string
db.query(`SELECT * FROM users WHERE id = ${userId}`);  // ruleid: string-format-sql

// match: INSERT with interpolation
db.query(`INSERT INTO logs VALUES (${userId})`);  // ruleid: string-format-sql

// match: DELETE with interpolation
db.query(`DELETE FROM sessions WHERE user = ${userId}`);  // ruleid: string-format-sql

// no-match: plain string literal (no interpolation)
db.query(`SELECT * FROM users WHERE id = ?`);  // ok: string-format-sql

// no-match: non-SQL template string
const greeting = `Hello, ${userId}!`;  // ok: string-format-sql
