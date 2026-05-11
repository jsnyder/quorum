import * as crypto from 'crypto';

// TP: should match
const b1 = crypto.randomBytes(4); // ruleid: ts-weak-crypto-random
const b2 = crypto.randomBytes(7); // ruleid: ts-weak-crypto-random
const b3 = crypto.randomBytes(0); // ruleid: ts-weak-crypto-random

// FP: should NOT match
const b4 = crypto.randomBytes(8); // ok: ts-weak-crypto-random
const b5 = crypto.randomBytes(16); // ok: ts-weak-crypto-random
const b6 = crypto.randomBytes(num); // ok: ts-weak-crypto-random
