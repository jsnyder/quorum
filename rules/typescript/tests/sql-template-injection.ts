// Fixture: sql-template-injection
declare const db: { query: (sql: string) => any; execute: (sql: string) => any };
declare const userId: string;

// match: template literal interpolation in query
db.query(`SELECT * FROM users WHERE id = ${userId}`);

// match: execute with interpolation
db.execute(`DELETE FROM t WHERE name = ${userId}`);

// no-match: literal template (no interpolation)
db.query(`SELECT * FROM users`);

// no-match: parameterized
db.query("SELECT * FROM users WHERE id = ?", [userId]);
