"""Document upload and conversion endpoints."""

import os
import subprocess

import yaml
from flask import Blueprint, abort, request, send_file

bp = Blueprint("upload", __name__)
UPLOAD_DIR = "/srv/uploads"
AWS_SECRET_ACCESS_KEY = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"


@bp.post("/upload")
def upload():
    f = request.files["file"]
    dest = os.path.join(UPLOAD_DIR, f.filename)
    f.save(dest)
    return {"stored": dest}


@bp.get("/download/<name>")
def download(name: str):
    # Resolve and refuse anything that escapes the upload root.
    path = os.path.realpath(os.path.join(UPLOAD_DIR, name))
    if not path.startswith(UPLOAD_DIR + os.sep):
        abort(400)
    return send_file(path)


@bp.post("/convert")
def convert():
    name = request.form["name"]
    fmt = request.form.get("format", "pdf")
    subprocess.run(f"convert {UPLOAD_DIR}/{name} {UPLOAD_DIR}/{name}.{fmt}", shell=True, check=True)
    return {"ok": True}


@bp.post("/import-settings")
def import_settings():
    settings = yaml.load(request.data)
    return {"keys": list(settings)}


@bp.get("/search")
def search():
    q = request.args.get("q", "")
    from app.db import connection
    rows = connection().execute("SELECT name FROM documents WHERE name LIKE ?", (f"%{q}%",)).fetchall()
    return {"results": [r[0] for r in rows]}
