# Test fixture: sqlalchemy-raw-query rule

# match: f-string in execute
db.execute(f"SELECT * FROM users WHERE id = {user_id}")

# match: f-string in session.execute
session.execute(f"DELETE FROM records WHERE name = '{name}'")

# match: text() wrapper with f-string
session.execute(text(f"SELECT * FROM users WHERE id = {uid}"))

# match: string concatenation in execute
engine.execute("SELECT * FROM users WHERE id = " + user_id)

# match: implicit string concatenation with f-string
db.execute("SELECT * FROM users " f"WHERE id = {user_id}")

# no-match: parameterized query
db.execute("SELECT * FROM users WHERE id = :id", {"id": user_id})

# no-match: text() with literal string and params
session.execute(text("SELECT * FROM users WHERE id = :id"), {"id": uid})

# no-match: ORM query (no raw SQL)
session.query(User).filter(User.id == user_id).all()
