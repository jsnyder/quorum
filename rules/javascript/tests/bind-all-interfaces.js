// Fixture: bind-all-interfaces (js)
const http = require("http");
const server = http.createServer();

// match: listen to 0.0.0.0
server.listen(3000, "0.0.0.0");

// match: { host: '0.0.0.0' } option
const opts = { host: "0.0.0.0" };

// no-match: loopback
server.listen(3000, "127.0.0.1");

// no-match: unrelated host
const meta = { host: "api.example.com" };
