class CacheManager:
    _instance = None

    @classmethod
    def get_instance(cls):
        # TP: should match - non-threadsafe singleton
        # ruleid: non-threadsafe-singleton
        if cls._instance is None:
            cls._instance = cls()
        return cls._instance

class AnotherSingleton:
    _instance = None

    @classmethod
    def instance(cls):
        # TP: should match - with arguments
        # ruleid: non-threadsafe-singleton
        if cls._instance is None:
            cls._instance = cls(config=default_config)
        return cls._instance

# FP: should NOT match - regular None check
if value is None:
    value = compute()

# FP: should NOT match - self._instance
if self._instance is None:
    self._instance = create()
