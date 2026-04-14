// Test fixture: path-traversal-join rule
import * as path from 'path';

// match: path.join with req.params
const filePath = path.join(uploadsDir, req.params.filename);

// match: path.join with req.query
const doc = path.join('/var/data', req.query.path);

// match: path.join with req.body
const target = path.join(baseDir, req.body.filePath);

// match: path.resolve with req.params
const abs = path.resolve(req.params.file);

// match: path.join with multiple args and req.query
const nested = path.join(root, 'sub', req.query.name);

// no-match: path.join with string literals only
const safe = path.join('/var', 'data', 'config.json');

// no-match: path.join with local variable (not req)
const local = path.join(baseDir, userVar);

// no-match: path.resolve with literal
const abs2 = path.resolve('/etc/config');
