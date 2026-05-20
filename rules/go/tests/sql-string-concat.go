package test

import "database/sql"

// Should trigger:
func bad(db *sql.DB, user string) {
	db.Query("SELECT * FROM users WHERE name = '" + user + "'")
}

// Should NOT trigger:
func good(db *sql.DB, user string) {
	db.Query("SELECT * FROM users WHERE name = $1", user)
}
