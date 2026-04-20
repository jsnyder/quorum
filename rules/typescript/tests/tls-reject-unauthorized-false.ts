// Fixture: tls-reject-unauthorized-false
import https from "https";

// match: inline false disables cert validation
const agent = new https.Agent({ rejectUnauthorized: false });

// match: quoted key false
const opts = { "rejectUnauthorized": false };

// no-match: true (validation on)
const ok = new https.Agent({ rejectUnauthorized: true });

// no-match: unrelated option
const other = { keepAlive: false };
