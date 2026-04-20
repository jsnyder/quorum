# Fixture: flask-debug-true
from flask import Flask

# match: debug=True in app.run
app = Flask(__name__)
app.run(debug=True)

# match: debug=True in Flask constructor-like call
app2 = Flask(__name__, debug=True)

# no-match: debug=False
app.run(debug=False)

# no-match: no debug kwarg
app.run(port=5000)
