#!/usr/bin/env python3
"""Drive per-language pattern mining against LiteLLM (Gemini 2.5 Pro, OpenAI-compatible)."""
import json, os, sys, time, urllib.request

BASE = os.environ["QUORUM_BASE_URL"].rstrip("/")
KEY  = os.environ["QUORUM_API_KEY"]
MODEL = os.environ.get("MINING_MODEL", "gemini-2.5-pro")

HERE = os.path.dirname(os.path.abspath(__file__))
PROMPT_MD = open(os.path.join(HERE, "PROMPT.md")).read()

# Split the prompt doc into system + user template
SYSTEM = PROMPT_MD.split("## System", 1)[1].split("## User", 1)[0].strip()
USER_TPL = PROMPT_MD.split("## User", 1)[1].split("## Your task", 1)[0] \
           + "## Your task" + PROMPT_MD.split("## Your task", 1)[1]
USER_TPL = USER_TPL.strip()

def call(lang, payload):
    existing = payload.get("existing_astgrep_rules", [])
    user = USER_TPL.replace("{LANGUAGE}", lang) \
                   .replace("{EXISTING_RULES}", "\n".join(f"- {r}" for r in existing) or "(none)") \
                   .replace("{{INPUT_JSON}}", json.dumps(payload, indent=2))
    body = {
        "model": MODEL,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user",   "content": user},
        ],
        "response_format": {"type": "json_object"},
        "temperature": 0.2,
    }
    req = urllib.request.Request(
        f"{BASE}/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"},
        method="POST",
    )
    t0 = time.time()
    with urllib.request.urlopen(req, timeout=600) as r:
        data = json.loads(r.read())
    dt = time.time() - t0
    msg = data["choices"][0]["message"]["content"]
    usage = data.get("usage", {})
    return msg, usage, dt

def main():
    langs = sys.argv[1:] or ["python","typescript","rust","yaml","bash","javascript"]
    for L in langs:
        inp = os.path.join(HERE, f"{L}.input.json")
        if not os.path.exists(inp):
            print(f"[skip] no input for {L}"); continue
        payload = json.loads(open(inp).read())
        print(f"[{L}] TP={payload['tp_count']} FP={payload['fp_count']} -> calling {MODEL}...", flush=True)
        try:
            msg, usage, dt = call(L, payload)
        except Exception as e:
            print(f"[{L}] ERROR: {e}")
            continue
        outp = os.path.join(HERE, f"{L}.candidates.json")
        open(outp, "w").write(msg)
        print(f"[{L}] {usage} ({dt:.1f}s) -> {outp} ({os.path.getsize(outp)} bytes)")

if __name__ == "__main__":
    main()
