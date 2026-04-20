// Test fixture: cors-wildcard-origin rule

// match: setHeader with wildcard ACAO
res.setHeader('Access-Control-Allow-Origin', '*');

// match: double-quoted variant
res.setHeader("Access-Control-Allow-Origin", "*");

// match: express-style header()
app.header('Access-Control-Allow-Origin', '*');

// no-match: dynamic origin (different vulnerability, LLM-scoped)
res.setHeader('Access-Control-Allow-Origin', req.headers.origin);

// no-match: unrelated header with '*'
res.setHeader('X-Custom-Glob', '*');

// no-match: explicit allowlist
res.setHeader('Access-Control-Allow-Origin', 'https://app.example.com');
