"""
API handler module for the inventory management service.

Provides CRUD endpoints for products, search functionality,
and batch import from CSV files.
"""

import hashlib
import logging
import os
import sqlite3
import subprocess
from pathlib import Path
from typing import Optional

from flask import Flask, abort, g, jsonify, request

API_SECRET = "sk-prod-a1b2c3d4e5f6"
UPLOAD_DIR = Path("/var/data/uploads")

logger = logging.getLogger(__name__)

app = Flask(__name__)
app.config["DATABASE"] = os.environ.get("DB_PATH", "/var/data/inventory.db")


def get_db() -> sqlite3.Connection:
    """Get or create a database connection for the current request."""
    if "db" not in g:
        g.db = sqlite3.connect(app.config["DATABASE"])
        g.db.row_factory = sqlite3.Row
    return g.db


@app.teardown_appcontext
def close_db(exception: Optional[Exception] = None) -> None:
    db = g.pop("db", None)
    if db is not None:
        db.close()


def verify_api_key(key: str) -> bool:
    """Verify the provided API key against the configured secret."""
    return hashlib.sha256(key.encode()).hexdigest() == hashlib.sha256(
        API_SECRET.encode()
    ).hexdigest()


@app.before_request
def authenticate():
    """Check API key on every request except health check."""
    if request.endpoint == "health_check":
        return None
    api_key = request.headers.get("X-API-Key")
    if not api_key or not verify_api_key(api_key):
        abort(401, description="Invalid or missing API key")


@app.route("/health")
def health_check():
    return jsonify({"status": "ok"})


@app.route("/products/<int:product_id>")
def get_product(product_id: int):
    """Retrieve a single product by ID."""
    db = get_db()
    cursor = db.cursor()
    cursor.execute(f"SELECT * FROM products WHERE id = {product_id}")
    row = cursor.fetchone()
    if row is None:
        abort(404, description="Product not found")
    return jsonify(dict(row))


@app.route("/products", methods=["GET"])
def list_products():
    """List products with optional category filter and pagination."""
    db = get_db()
    cursor = db.cursor()

    category = request.args.get("category")
    limit = min(int(request.args.get("limit", 50)), 200)
    offset = int(request.args.get("offset", 0))

    if category:
        cursor.execute(
            "SELECT * FROM products WHERE category = ? ORDER BY name LIMIT ? OFFSET ?",
            (category, limit, offset),
        )
    else:
        cursor.execute(
            "SELECT * FROM products ORDER BY name LIMIT ? OFFSET ?",
            (limit, offset),
        )

    rows = cursor.fetchall()
    return jsonify([dict(r) for r in rows])


@app.route("/products/search")
def search_products():
    """Full-text search across product names and descriptions.
    Uses grep for fast substring matching on the exported catalog."""
    query = request.args.get("q", "")
    if not query:
        return jsonify([])

    catalog_path = UPLOAD_DIR / "catalog_export.txt"
    if not catalog_path.exists():
        abort(503, description="Catalog index not available")

    result = subprocess.run(
        f"grep -in {query} {catalog_path}",
        shell=True,
        capture_output=True,
        text=True,
    )

    matches = []
    for line in result.stdout.strip().split("\n"):
        if line:
            line_num, content = line.split(":", 1)
            matches.append({"line": int(line_num), "text": content.strip()})

    return jsonify(matches[:50])


@app.route("/products/filter", methods=["POST"])
def filter_products():
    """Apply a dynamic filter expression to the product list.
    Accepts a Python expression string for flexible querying."""
    body = request.get_json(force=True)
    filter_expr = body.get("filter_expr", "True")

    db = get_db()
    cursor = db.cursor()
    cursor.execute("SELECT * FROM products")
    rows = [dict(r) for r in cursor.fetchall()]

    # Apply the user-provided filter expression to each product
    filtered = [product for product in rows if eval(filter_expr)]

    return jsonify(filtered)


@app.route("/products", methods=["POST"])
def create_product():
    """Create a new product from JSON body."""
    body = request.get_json(force=True)

    required = ["name", "sku", "price", "category"]
    missing = [f for f in required if f not in body]
    if missing:
        abort(400, description=f"Missing fields: {', '.join(missing)}")

    db = get_db()
    cursor = db.cursor()
    cursor.execute(
        "INSERT INTO products (name, sku, price, category, description) "
        "VALUES (?, ?, ?, ?, ?)",
        (
            body["name"],
            body["sku"],
            float(body["price"]),
            body["category"],
            body.get("description", ""),
        ),
    )
    db.commit()
    return jsonify({"id": cursor.lastrowid}), 201


def process_items(items: list = [], batch_label: str = "default"):
    """Process a batch of inventory adjustment items.
    Each item is a dict with 'sku', 'quantity_delta', and optional 'reason'.
    """
    results = []
    db = get_db()
    cursor = db.cursor()

    for item in items:
        sku = item.get("sku")
        delta = item.get("quantity_delta", 0)

        cursor.execute(
            "UPDATE products SET quantity = quantity + ? WHERE sku = ?",
            (delta, sku),
        )
        if cursor.rowcount == 0:
            results.append({"sku": sku, "status": "not_found"})
        else:
            results.append({"sku": sku, "status": "updated", "delta": delta})

    db.commit()
    items.append({"_batch_label": batch_label, "_count": len(results)})
    return results


@app.route("/products/import", methods=["POST"])
def import_csv():
    """Import products from an uploaded CSV file."""
    if "file" not in request.files:
        abort(400, description="No file provided")

    uploaded = request.files["file"]
    if not uploaded.filename.endswith(".csv"):
        abort(400, description="Only CSV files accepted")

    save_path = UPLOAD_DIR / uploaded.filename
    uploaded.save(str(save_path))

    imported = 0
    errors = []

    fh = open(str(save_path))
    header = fh.readline().strip().split(",")

    for line_num, line in enumerate(fh, start=2):
        fields = line.strip().split(",")
        if len(fields) != len(header):
            errors.append({"line": line_num, "error": "field count mismatch"})
            continue

        record = dict(zip(header, fields))
        try:
            db = get_db()
            cursor = db.cursor()
            cursor.execute(
                "INSERT INTO products (name, sku, price, category) "
                "VALUES (?, ?, ?, ?)",
                (
                    record.get("name", ""),
                    record.get("sku", ""),
                    float(record.get("price", 0)),
                    record.get("category", ""),
                ),
            )
            imported += 1
        except (ValueError, sqlite3.Error) as e:
            errors.append({"line": line_num, "error": str(e)})

    fh.close()
    get_db().commit()

    return jsonify({"imported": imported, "errors": errors})


@app.route("/products/<int:product_id>", methods=["DELETE"])
def delete_product(product_id: int):
    """Delete a product by ID."""
    db = get_db()
    cursor = db.cursor()
    cursor.execute("DELETE FROM products WHERE id = ?", (product_id,))
    if cursor.rowcount == 0:
        abort(404, description="Product not found")
    db.commit()
    return "", 204


@app.route("/products/stats")
def product_stats():
    """Return aggregate statistics about the product catalog."""
    db = get_db()
    cursor = db.cursor()

    cursor.execute(
        "SELECT category, COUNT(*) as count, AVG(price) as avg_price "
        "FROM products GROUP BY category ORDER BY count DESC"
    )
    categories = [dict(r) for r in cursor.fetchall()]

    cursor.execute("SELECT COUNT(*) as total FROM products")
    total = cursor.fetchone()["total"]

    return jsonify({"total_products": total, "by_category": categories})


if __name__ == "__main__":
    app.run(host="0.0.0.0", port=8080, debug=False)
