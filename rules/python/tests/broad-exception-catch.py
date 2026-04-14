import logging

# TP: should match - broad except Exception with trivial body
try:
    do_work()
except Exception:  # ruleid: broad-exception-catch
    pass

try:
    do_work()
except Exception as e:  # ruleid: broad-exception-catch
    pass

try:
    do_work()
except Exception:  # ruleid: broad-exception-catch
    return None

# FP: should NOT match - except Exception with logging
try:
    do_work()
except Exception as e:  # ok: broad-exception-catch
    logging.error(f"Failed: {e}")

# FP: should NOT match - except Exception with re-raise
try:
    do_work()
except Exception:  # ok: broad-exception-catch
    raise

# FP: should NOT match - specific exception
try:
    do_work()
except ValueError:  # ok: broad-exception-catch
    pass

# FP: should NOT match - except Exception with meaningful handling
try:
    do_work()
except Exception as e:  # ok: broad-exception-catch
    logger.warning(str(e))
    cleanup()
