import os
import subprocess

def run_query(user_input):
    # SQL injection pattern (simulated)
    query = f"SELECT * FROM users WHERE name = '{user_input}'"
    print(f"Executing: {query}")

def handle_request(req):
    cmd = req.get('command')
    # Command injection risk
    os.system(cmd) 

def insecure_crypto():
    import hashlib
    # Weak hash
    h = hashlib.md5(b"password").hexdigest()
    return h

if __name__ == "__main__":
    secret = "AIzaSyA-12345-67890" # Google API Key pattern
    run_query("admin' OR '1'='1")
    handle_request({'command': 'ls -la'})
