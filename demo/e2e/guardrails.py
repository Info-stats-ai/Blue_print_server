"""End-to-end check of the sharing guardrails and sync tracking against a real
local server: secret files are refused, keys in chat are masked, file
receipts and outlines of unsent files reach teammates, and the daily AI
spend survives a restart.

Run it after `cargo build`, with the websockets package (12.x) installed:
    python3 -m venv /tmp/wsenv && /tmp/wsenv/bin/pip install websockets==12.0
    /tmp/wsenv/bin/python demo/e2e/guardrails.py
"""
import asyncio, json, os, sqlite3, subprocess, sys, tempfile, time, urllib.request
import websockets

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agent_runs import Peer, call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

SERVER = os.path.expanduser("~/blueprint-server/target/debug/blueprint-server")
PORT = 3919
STATE = tempfile.mkdtemp(prefix="guard-")
DB_PATH = f"{STATE}/e2e.db"
agent_runs.PORT = PORT
agent_runs.BASE = f"http://127.0.0.1:{PORT}"

KEY = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx"


def start_server():
    env = dict(os.environ, DATABASE_URL=f"sqlite:{DB_PATH}", JWT_SECRET="e2e-local-only", PORT=str(PORT))
    env.pop("ANTHROPIC_API_KEY", None)
    env.pop("VOYAGE_API_KEY", None)
    proc = subprocess.Popen([SERVER], env=env, cwd=STATE, stdout=open(f"{STATE}/server.log", "a"), stderr=subprocess.STDOUT)
    for _ in range(80):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/robots.txt", timeout=1)
            return proc
        except Exception:
            time.sleep(0.25)
    raise SystemExit("server didn't start; see " + STATE)


def saved(message, path):
    return message.get("type") == "file_saved" and message.get("path") == path


async def main():
    server = start_server()
    alice = call("POST", "/auth/register", {"email": "alice@example.test", "password": "e2e-only-password"})
    bob = call("POST", "/auth/register", {"email": "bob@example.test", "password": "e2e-only-password"})
    created = call("POST", "/sessions", {"name": "Guard", "goal": "Test guardrails", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    call("POST", f"/sessions/{session}/join", {"invite": invite}, bob["token"])
    call("PUT", f"/sessions/{session}/join-requests/{bob['user_id']}", {"approve": True}, alice["token"])

    a, b = Peer("alice", alice["token"], session), Peer("bob", bob["token"], session)
    await a.connect()
    await b.connect()

    # An ordinary file goes through.
    await a.send({"type": "file_save", "path": "shoplite/cart.py", "content": "class Cart:\n    pass\n"})
    check("An ordinary file reaches Bob", await b.wait(lambda m: saved(m, "shoplite/cart.py")) is not None)

    # Secret files, files holding keys, and paths outside the project don't,
    # and only the sender is told why.
    for path, content, why in [
        (".env", "A=1", "usually holds keys"),
        ("config/settings.py", f"KEY = '{KEY}'", "looks like it contains a key"),
        ("../outside.py", "x = 1", "isn't a path inside the project"),
        ("deploy/id_rsa", "x", "usually holds keys"),
    ]:
        await a.send({"type": "file_save", "path": path, "content": content})
        notice = await a.wait(lambda m: m.get("type") == "notice")
        check(f"{path}: Alice is told it wasn't shared", notice is not None and why in notice["message"], notice)
        leaked = await b.wait(lambda m: saved(m, path), timeout=0.6)
        check(f"{path}: never reaches Bob", leaked is None)

    # Live editing of a secret file is refused too.
    await a.send({"type": "buffer_join", "path": ".env.local"})
    notice = await a.wait(lambda m: m.get("type") == "notice")
    check("Live editing a secrets file is refused", notice is not None, notice)

    # A key pasted into chat is masked before anyone sees or stores it.
    await a.send({"type": "chat", "text": f"use {KEY} for now"})
    chat = await b.wait(lambda m: m.get("type") == "chat")
    check("Keys in chat are masked for Bob", chat is not None and KEY not in json.dumps(chat) and "[redacted secret]" in json.dumps(chat), chat)
    rows = sqlite3.connect(DB_PATH).execute("SELECT text FROM chat_messages").fetchall()
    check("Keys in chat are masked in the database", rows and all(KEY not in row[0] for row in rows), rows)

    # Agent steps and pinned decisions are masked too.
    await a.send({"type": "agent_step", "agent": "agent", "kind": "action", "text": f"ran deploy with {KEY}", "remember": True})
    step = await b.wait(lambda m: m.get("type") == "agent_step")
    check("Keys in agent steps are masked", step is not None and KEY not in step["text"], step)
    await a.send({"type": "pin_decision", "text": f"Staging key is {KEY}"})
    decisions = await b.wait(lambda m: m.get("type") == "decisions")
    check("Keys in pinned decisions are masked", decisions is not None and KEY not in json.dumps(decisions), decisions)

    # Sync tracking: Bob confirms he has the file, and Alice hears it.
    await b.send({"type": "file_ack", "path": "shoplite/cart.py", "applied": False})
    ack = await a.wait(lambda m: m.get("type") == "file_ack")
    check("Alice hears Bob is reviewing her file", ack is not None and ack["user_id"] == bob["user_id"] and ack["applied"] is False, ack)

    # Outlines of unsent files: signatures reach teammates, masked.
    await a.send({"type": "file_preview", "path": "shoplite/catalog.py",
                  "outline": ["class Catalog", "def add(self, item, name, price)", f"API = '{KEY}'"]})
    preview = await b.wait(lambda m: m.get("type") == "file_preview")
    check("Bob gets the outline of Alice's unsent file", preview is not None and "def add(self, item, name, price)" in preview["outline"], preview)
    check("Outlines are masked", preview is not None and KEY not in json.dumps(preview), preview)
    await a.send({"type": "file_preview", "path": ".env", "outline": ["SECRET"]})
    check("No outline for a secrets file", await b.wait(lambda m: m.get("type") == "file_preview", timeout=0.6) is None)

    # A message larger than the limit is refused by the connection.
    try:
        await a.send({"type": "chat", "text": "x" * (7 * 1024 * 1024)})
        await asyncio.sleep(0.5)
        await a.send({"type": "chat", "text": "still here?"})
        alive = await b.wait(lambda m: m.get("type") == "chat" and "still here" in m.get("text", ""), timeout=1.5)
    except Exception:
        alive = None
    check("An oversized message is refused", alive is None)

    await b.close()
    server.terminate()
    server.wait()

    # The daily AI spend survives a restart.
    today = time.strftime("%Y-%m-%d", time.gmtime())
    connection = sqlite3.connect(DB_PATH)
    connection.execute("INSERT INTO ai_spend (scope, day, usd) VALUES (?, ?, 5.0)", (session, today))
    connection.commit()
    connection.close()
    server = start_server()
    c = Peer("alice", alice["token"], session)
    snapshot = await c.connect()
    metrics = (snapshot or {}).get("metrics", {})
    check("Today's spend is restored after a restart", metrics.get("spent_today_usd") == 5.0, metrics)
    check("The daily cap is shown", metrics.get("daily_cap_usd") == 2.0, metrics)
    await c.send({"type": "chat", "text": "@agent hello"})
    reply = await c.wait(lambda m: m.get("type") == "chat" and m.get("sender") == "agent", timeout=3)
    check("Over the cap, the shared AI says so instead of calling the model",
          reply is not None and "today's limit" in reply.get("text", ""), reply)
    await c.close()
    server.terminate()
    server.wait()

    print()
    print("ALL PASSED" if not failures else f"{len(failures)} FAILED: {', '.join(failures)}")
    sys.exit(1 if failures else 0)


asyncio.run(main())
