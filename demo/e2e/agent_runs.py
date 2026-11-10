"""End-to-end check of agent runs against a real local server: Alice's agent
runs a task, Bob reviews it, and remembered runs survive a restart.

Run it after `cargo build`, with the websockets package (12.x) installed:
    python3 -m venv /tmp/wsenv && /tmp/wsenv/bin/pip install websockets==12.0
    /tmp/wsenv/bin/python demo/e2e/agent_runs.py
"""
import asyncio, json, os, subprocess, sys, tempfile, time, urllib.request
import websockets

SERVER = os.path.expanduser("~/blueprint-server/target/debug/blueprint-server")
PORT = 3918
BASE = f"http://127.0.0.1:{PORT}"
STATE = tempfile.mkdtemp(prefix="adlc-")
DB = f"sqlite:{STATE}/e2e.db"
failures = []


def check(label, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + label + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(label)


def start_server():
    env = dict(os.environ, DATABASE_URL=DB, JWT_SECRET="e2e-local-only", PORT=str(PORT))
    env.pop("ANTHROPIC_API_KEY", None)
    proc = subprocess.Popen([SERVER], env=env, cwd=STATE, stdout=open(f"{STATE}/server.log", "a"), stderr=subprocess.STDOUT)
    for _ in range(80):
        try:
            urllib.request.urlopen(f"{BASE}/robots.txt", timeout=1)
            return proc
        except Exception:
            time.sleep(0.25)
    raise SystemExit("server didn't start; see " + STATE)


def call(method, path, body=None, token=None):
    request = urllib.request.Request(BASE + path, method=method, data=json.dumps(body).encode() if body is not None else None)
    request.add_header("content-type", "application/json")
    if token:
        request.add_header("authorization", f"Bearer {token}")
    with urllib.request.urlopen(request) as response:
        text = response.read().decode()
        return json.loads(text) if text else None


class Peer:
    def __init__(self, name, token, session):
        self.name, self.token, self.session, self.inbox = name, token, session, []

    async def connect(self):
        self.ws = await websockets.connect(
            f"ws://127.0.0.1:{PORT}/ws?session_id={self.session}",
            extra_headers={"authorization": f"Bearer {self.token}"},
            max_size=None,
        )
        self.reader = asyncio.create_task(self._read())
        return await self.wait(lambda m: m.get("type") == "snapshot")

    async def _read(self):
        try:
            async for raw in self.ws:
                self.inbox.append(json.loads(raw))
        except Exception:
            pass

    async def send(self, message):
        await self.ws.send(json.dumps(message))

    async def wait(self, match, timeout=3.0):
        deadline = time.time() + timeout
        while time.time() < deadline:
            for index, message in enumerate(self.inbox):
                if match(message):
                    return self.inbox.pop(index)
            await asyncio.sleep(0.05)
        return None

    async def close(self):
        await self.ws.close()


def run_update(message, run_id, **want):
    if message.get("type") != "agent_run" or message["run"]["run_id"] != run_id:
        return False
    return all(message["run"].get(key) == value for key, value in want.items())


async def main():
    server = start_server()
    alice = call("POST", "/auth/register", {"email": "alice@example.test", "password": "e2e-only-password"})
    bob = call("POST", "/auth/register", {"email": "bob@example.test", "password": "e2e-only-password"})
    created = call("POST", "/sessions", {"name": "ADLC", "goal": "Test agent runs", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    call("POST", f"/sessions/{session}/join", {"invite": invite}, bob["token"])
    call("PUT", f"/sessions/{session}/join-requests/{bob['user_id']}", {"approve": True}, alice["token"])

    a, b = Peer("alice", alice["token"], session), Peer("bob", bob["token"], session)
    await a.connect()
    await b.connect()

    # Plan: Alice's agent starts a run, and its goal step is tagged with it.
    await a.send({"type": "agent_run", "run_id": "run-1", "agent": "I-mode agent", "goal": "Add a due date to tasks", "status": "running", "remember": True})
    seen = await b.wait(lambda m: run_update(m, "run-1", status="running"))
    check("Bob sees Alice's run start", seen is not None and seen["run"]["user_id"] == alice["user_id"])
    await a.send({"type": "agent_step", "agent": "I-mode agent", "kind": "plan", "text": "Add a due date to tasks", "run_id": "run-1", "remember": True})
    step = await b.wait(lambda m: m.get("type") == "agent_step")
    check("Steps carry their run id", step is not None and step.get("run_id") == "run-1", step)

    # Review is only for finished runs.
    await b.send({"type": "agent_run_review", "user_id": alice["user_id"], "run_id": "run-1", "verdict": "accepted"})
    error = await b.wait(lambda m: m.get("type") == "error")
    check("A running run can't be reviewed", error is not None and "hasn't finished" in error["message"], error)

    # Finish.
    await a.send({"type": "agent_run", "run_id": "run-1", "agent": "I-mode agent", "goal": "changed goal is ignored", "status": "done",
                  "files": ["src/tasks.rs", "src/tasks.rs", "tests/due.rs"], "summary": "Added due dates and two tests.", "remember": True})
    done = await b.wait(lambda m: run_update(m, "run-1", status="done"))
    check("Bob sees the run finish", done is not None)
    if done:
        check("The goal never changes", done["run"]["goal"] == "Add a due date to tasks", done["run"]["goal"])
        check("Files are de-duplicated", done["run"]["files"] == ["src/tasks.rs", "tests/due.rs"], done["run"]["files"])
        check("The remember flag isn't sent to clients", "remember" not in done["run"])

    # Nobody reviews their own run.
    await a.send({"type": "agent_run_review", "user_id": alice["user_id"], "run_id": "run-1", "verdict": "accepted"})
    error = await a.wait(lambda m: m.get("type") == "error")
    check("Alice can't review her own run", error is not None and "own" in error["message"], error)

    # Review, then change one's mind.
    await b.send({"type": "agent_run_review", "user_id": alice["user_id"], "run_id": "run-1", "verdict": "accepted"})
    reviewed = await a.wait(lambda m: run_update(m, "run-1") and m["run"]["reviews"])
    check("Alice sees Bob's acceptance", reviewed is not None and reviewed["run"]["reviews"][0]["verdict"] == "accepted"
          and reviewed["run"]["reviews"][0]["reviewer_id"] == bob["user_id"], reviewed)
    await b.send({"type": "agent_run_review", "user_id": alice["user_id"], "run_id": "run-1", "verdict": "changes", "note": "Needs a migration"})
    changed = await a.wait(lambda m: run_update(m, "run-1") and m["run"]["reviews"] and m["run"]["reviews"][0]["verdict"] == "changes")
    check("A new verdict replaces the old one", changed is not None and len(changed["run"]["reviews"]) == 1
          and changed["run"]["reviews"][0]["note"] == "Needs a migration", changed)

    # A finished run can't start again.
    await a.send({"type": "agent_run", "run_id": "run-1", "agent": "I-mode agent", "goal": "x", "status": "running"})
    restarted = await b.wait(lambda m: run_update(m, "run-1", status="running"), timeout=1.0)
    check("A finished run doesn't restart", restarted is None)

    # Bad input is dropped.
    await a.send({"type": "agent_run", "run_id": "<script>", "agent": "x", "goal": "x", "status": "running"})
    await a.send({"type": "agent_run", "run_id": "run-2", "agent": "x", "goal": "x", "status": "sleeping"})
    junk = await b.wait(lambda m: m.get("type") == "agent_run" and m["run"]["run_id"] in ("<script>", "run-2"), timeout=1.0)
    check("Bad run ids and statuses are dropped", junk is None, junk)

    # A live-only run (remember off).
    await a.send({"type": "agent_run", "run_id": "run-3", "agent": "I-mode agent", "goal": "Try something quick", "status": "done", "remember": False})
    check("Live-only runs are shared too", await b.wait(lambda m: run_update(m, "run-3")) is not None)

    # A late joiner gets runs in the snapshot.
    late = Peer("bob2", bob["token"], session)
    snapshot = await late.connect()
    ids = [run["run_id"] for run in snapshot.get("agent_runs", [])]
    check("The snapshot lists recent runs", ids == ["run-1", "run-3"], ids)
    await late.close()

    # Remember: after a restart only the remembered run comes back, with its review.
    for peer in (a, b):
        await peer.close()
    server.terminate()
    server.wait()
    server = start_server()
    again = Peer("bob3", bob["token"], session)
    snapshot = await again.connect()
    runs = snapshot.get("agent_runs", [])
    check("Only the remembered run survives a restart", [run["run_id"] for run in runs] == ["run-1"], [run["run_id"] for run in runs])
    if runs:
        check("Its review survives too", runs[0]["reviews"] and runs[0]["reviews"][0]["verdict"] == "changes", runs[0])
        check("Its files survive too", runs[0]["files"] == ["src/tasks.rs", "tests/due.rs"], runs[0]["files"])
    await again.close()
    server.terminate()
    print(f"\n{'ALL PASSED' if not failures else str(len(failures)) + ' FAILED'}  (state in {STATE})")
    sys.exit(1 if failures else 0)


asyncio.run(main())
