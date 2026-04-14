import os

# TP: should match - world-writable permissions
os.chmod("/tmp/myfile", 0o777)  # ruleid: insecure-file-permissions
os.chmod(filepath, 0o777)  # ruleid: insecure-file-permissions
os.chmod("/tmp/myfile", 0o666)  # ruleid: insecure-file-permissions

# FP: should NOT match - restrictive permissions
os.chmod("/tmp/myfile", 0o755)  # ok: insecure-file-permissions
os.chmod("/tmp/myfile", 0o600)  # ok: insecure-file-permissions
os.chmod("/tmp/myfile", 0o644)  # ok: insecure-file-permissions
os.chmod(filepath, mode)  # ok: insecure-file-permissions
