# Fixture: bind-all-interfaces
from flask import Flask
import uvicorn

app = Flask(__name__)

# match: Flask exposed on all interfaces
app.run(host="0.0.0.0", port=8080)

# match: uvicorn keyword form
uvicorn.run("main:app", host="0.0.0.0")

# no-match: loopback only
app.run(host="127.0.0.1", port=8080)

# no-match: no host argument
app.run(port=5000)
