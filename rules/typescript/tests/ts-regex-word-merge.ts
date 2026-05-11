// TP: should match
const s1 = text.replace(/[^a-z0-9]/g, ''); // ruleid: ts-regex-word-merge
const s2 = text.replace(/[\W_]/g, ''); // ruleid: ts-regex-word-merge

// FP: should NOT match
const s3 = text.replace(/[^a-z0-9]/g, ' '); // ok: ts-regex-word-merge
const s4 = text.replace(/foo/g, ''); // ok: ts-regex-word-merge
const s5 = text.replace(/[a-z]/g, ''); // ok: ts-regex-word-merge
