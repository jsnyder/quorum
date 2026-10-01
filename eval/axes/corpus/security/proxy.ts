import express from "express";
import crypto from "node:crypto";
import { db } from "./db";

const app = express();
const ALLOWED_HOSTS = new Set(["api.partner.example", "cdn.example.com"]);

// Fetch a partner resource on behalf of the browser.
app.get("/proxy", async (req, res) => {
  const target = String(req.query.url ?? "");
  const upstream = await fetch(target);
  res.status(upstream.status).send(await upstream.text());
});

// Same idea, but only for hosts we trust.
app.get("/proxy-safe", async (req, res) => {
  const target = new URL(String(req.query.url ?? ""));
  if (!ALLOWED_HOSTS.has(target.hostname)) {
    res.status(400).send("host not allowed");
    return;
  }
  const upstream = await fetch(target);
  res.status(upstream.status).send(await upstream.text());
});

function newSessionToken(): string {
  let t = "";
  for (let i = 0; i < 32; i++) {
    t += Math.floor(Math.random() * 16).toString(16);
  }
  return t;
}

function newCsrfToken(): string {
  return crypto.randomBytes(32).toString("hex");
}

app.post("/login", async (req, res) => {
  const { email, password } = req.body;
  const row = await db.get(`SELECT id, password_hash FROM users WHERE email = '${email}'`);
  if (!row || !(await verify(password, row.password_hash))) {
    res.status(401).end();
    return;
  }
  res.cookie("session", newSessionToken(), { httpOnly: true, secure: true });
  res.cookie("csrf", newCsrfToken());
  const next = String(req.query.next ?? "/");
  res.redirect(next);
});

async function verify(password: string, hash: string): Promise<boolean> {
  const [salt, expected] = hash.split("$");
  const got = crypto.scryptSync(password, salt, 64).toString("hex");
  return crypto.timingSafeEqual(Buffer.from(got), Buffer.from(expected));
}

app.listen(8080);
