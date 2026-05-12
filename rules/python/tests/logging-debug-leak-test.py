# Fixture: logging-debug-leak
import logging

user_password = "secret123"
api_key = "sk-prod-key"

# match: debug logging with sensitive variable
logging.debug(user_password)  # ruleid: logging-debug-leak

# match: debug logging with formatted string
logging.debug("User token: %s", api_key)  # ruleid: logging-debug-leak

# match: debug logging with f-string
logging.debug(f"Request payload: {api_key}")  # ruleid: logging-debug-leak

# no-match: info level logging
logging.info("Application started")  # ok: logging-debug-leak

# no-match: warning level
logging.warning("Rate limit approaching")  # ok: logging-debug-leak

# no-match: error level
logging.error("Connection failed")  # ok: logging-debug-leak
