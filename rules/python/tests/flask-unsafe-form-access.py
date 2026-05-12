from flask import request

# TP: should match
username = request.form['username']  # ruleid: flask-unsafe-form-access
password = request.form['password']  # ruleid: flask-unsafe-form-access

# FP: should NOT match
username = request.form.get('username')  # ok: flask-unsafe-form-access
password = request.form.get('password', 'default')  # ok: flask-unsafe-form-access
