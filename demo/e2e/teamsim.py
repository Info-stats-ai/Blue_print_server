"""Plays teammates in a running demo session, the way their apps would talk to
the server, for multi-person tests when only some windows can be driven.

    teamsim.py chat dave "Decision: loans are due after 14 days"
    teamsim.py work carol 300          # Carol's agent works on store.py for 300 s
    teamsim.py review dave alice "thread" changes "Fines should be in cents"

Uses the demo's local server and accounts (see start-demo.sh).
"""
import asyncio, functools, json, sqlite3, sys, time, urllib.request

print = functools.partial(print, flush=True)
from pathlib import Path

import websockets

BASE = "http://127.0.0.1:3917"
STATE = Path(__file__).resolve().parent.parent / ".state"


def login(name):
    request = urllib.request.Request(
        BASE + "/auth/login",
        data=json.dumps({"email": f"{name}@example.test", "password": "demo-only-password"}).encode(),
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request) as response:
        return json.loads(response.read())["token"]


def user_id(name):
    db = sqlite3.connect(STATE / "demo.db")
    return db.execute("select id from users where email = ?", (f"{name}@example.test",)).fetchone()[0]


async def connect(name):
    session = (STATE / "session_id").read_text().strip()
    ws = await websockets.connect(
        f"ws://127.0.0.1:3917/ws?session_id={session}",
        extra_headers={"authorization": f"Bearer {login(name)}"},
        max_size=None,
    )
    snapshot = None
    while snapshot is None:
        message = json.loads(await ws.recv())
        if message.get("type") == "snapshot":
            snapshot = message
    return ws, snapshot


async def chat(name, text):
    ws, _ = await connect(name)
    await ws.send(json.dumps({"type": "chat", "text": text}))
    await asyncio.sleep(1)
    await ws.close()
    print(f"{name} said: {text}")


async def work(name, seconds):
    """Carol's agent saves the store to disk: claims its part of store.py,
    reports its run and steps, and lets go when done."""
    ws, _ = await connect(name)
    run_id = f"run-{int(time.time())}"
    goal = "Save and load the whole store as JSON (Store.save / Store.load)"
    send = lambda message: ws.send(json.dumps(message))
    await send({"type": "agent_run", "run_id": run_id, "agent": "I-mode agent", "goal": goal, "status": "running"})
    await send({"type": "agent_step", "agent": "I-mode agent", "kind": "plan", "text": goal, "run_id": run_id})
    await send({"type": "set_claims", "claims": [{
        "path": "bookshelf/store.py", "holder": "I-mode agent",
        "symbols": ["Store.__init__", "Store.save", "Store.load"],
    }]})
    await send({"type": "agent_step", "agent": "I-mode agent", "kind": "action",
                "text": "edit_file: bookshelf/store.py (adding Store.save and Store.load; changing Store.__init__ to take a path)",
                "path": "bookshelf/store.py", "run_id": run_id})
    print(f"{name}'s agent is working on store.py for {seconds}s")
    await asyncio.sleep(seconds)
    await send({"type": "set_claims", "claims": []})
    await send({"type": "agent_run", "run_id": run_id, "agent": "I-mode agent", "goal": goal, "status": "done",
                "files": ["bookshelf/store.py"], "summary": "Store.save(path) and Store.load(path) write and read JSON."})
    await asyncio.sleep(1)
    await ws.close()
    print(f"{name}'s agent finished")


async def review(name, owner, agent_contains, verdict, note):
    ws, snapshot = await connect(name)
    owner_id = user_id(owner)
    runs = [run for run in snapshot.get("agent_runs", [])
            if run["user_id"] == owner_id and agent_contains in run["agent"] and run["status"] != "running"]
    if not runs:
        print("no matching finished run"); await ws.close(); return
    run = runs[-1]
    await ws.send(json.dumps({"type": "agent_run_review", "user_id": owner_id, "run_id": run["run_id"],
                              "verdict": verdict, "note": note or None}))
    await asyncio.sleep(1)
    await ws.close()
    print(f"{name} marked {owner}'s \"{run['goal'][:60]}\" as {verdict}")


command, *args = sys.argv[1:]
if command == "chat":
    asyncio.run(chat(args[0], args[1]))
elif command == "work":
    asyncio.run(work(args[0], int(args[1])))
elif command == "review":
    asyncio.run(review(args[0], args[1], args[2], args[3], args[4] if len(args) > 4 else ""))
