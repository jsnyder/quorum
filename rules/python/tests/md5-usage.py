import hashlib

# TP: should match - MD5 usage
digest = hashlib.md5(data.encode())  # ruleid: md5-usage
hash = hashlib.md5()  # ruleid: md5-usage
hashlib.md5(b"hello world")  # ruleid: md5-usage

# FP: should NOT match - safe hash algorithms
digest = hashlib.sha256(data)  # ok: md5-usage
digest = hashlib.sha512(data.encode())  # ok: md5-usage
digest = hashlib.sha1(data)  # ok: md5-usage
