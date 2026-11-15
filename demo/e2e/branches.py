"""End-to-end check of branch lanes against a real local server: files sync
only between people on the same git branch, people on another branch get the
change as context (never as a file to write), and each branch keeps its own
catch-up, version history and live typing rules.

Run it after `cargo build`, with the websockets package (12.x) installed:
    python3 -m venv /tmp/wsenv && /tmp/wsenv/bin/pip install websockets==12.0
    /tmp/wsenv/bin/python demo/e2e/branches.py
"""
import asyncio, hashlib, os, sys, urllib.parse
import websockets

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agent_runs import Peer, call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

PORT = 3921
agent_runs.PORT = PORT
agent_runs.BASE = f"http://127.0.0.1:{PORT}"


class BranchPeer(Peer):
    """A client on a git branch, like the current app."""

    def __init__(self, name, token, session, branch):
        super().__init__(name, token, session)
        self.branch = branch

    async def connect(self):
        query = urllib.parse.urlencode({"session_id": self.session, "branch": self.branch})
        self.ws = await websockets.connect(
            f"ws://127.0.0.1:{PORT}/ws?{query}",
            extra_headers={"authorization": f"Bearer {self.token}"},
            max_size=None,
        )
        self.reader = asyncio.create_task(self._read())
        return await self.wait(lambda m: m.get("type") == "snapshot")


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def is_file(path, sender=None):
    return lambda m: (
        m.get("type") in ("file_saved", "file_patched")
        and m.get("path") == path
        and (sender is None or m.get("sender") == sender)
    )


def is_branch_file(path):
    return lambda m: m.get("type") == "branch_file" and m.get("path") == path


async def main():
    proc = agent_runs.start_server()
    try:
        await run()
    finally:
        proc.terminate()
    print("ALL PASSED" if not failures else f"{len(failures)} FAILED: {', '.join(failures)}")
    sys.exit(1 if failures else 0)


async def run():
    people = {}
    for name in ("alice", "bob", "carol", "dave"):
        people[name] = call("POST", "/auth/register", {"email": f"{name}@example.test", "password": "e2e-only-password"})
    alice = people["alice"]
    created = call("POST", "/sessions", {"name": "Branches", "goal": "Work on two branches", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    for name in ("bob", "carol", "dave"):
        person = people[name]
        call("POST", f"/sessions/{session}/join", {"invite": invite}, person["token"])
        call("PUT", f"/sessions/{session}/join-requests/{person['user_id']}", {"approve": True}, alice["token"])

    # Alice connects first, so her branch is the session's main one.
    a = BranchPeer("alice", alice["token"], session, "main")
    b = BranchPeer("bob", people["bob"]["token"], session, "main")
    c = BranchPeer("carol", people["carol"]["token"], session, "feature/tax")
    d = BranchPeer("dave", people["dave"]["token"], session, "feature/tax")
    snapshots = {}
    for peer in (a, b, c, d):
        snapshots[peer.name] = await peer.connect()
        check(f"{peer.name} connects", snapshots[peer.name] is not None)
    dave_snapshot = snapshots["dave"]
    branches = {entry["user_id"]: entry["branch"] for entry in dave_snapshot.get("branches", [])}
    check("the newest joiner's snapshot has everyone's branch and the main one",
          dave_snapshot.get("main_branch") == "main" and branches.get(people["carol"]["user_id"]) == "feature/tax"
          and branches.get(alice["user_id"]) == "main", str(dave_snapshot.get("branches")))
    told = await a.wait(lambda m: m.get("type") == "branch_info" and m.get("user_id") == people["carol"]["user_id"])
    check("people already there hear a newcomer's branch", told is not None and told["branch"] == "feature/tax")

    base = "class Cart:\n    def total(self):\n        return sum(self.items.values())\n"
    await a.send({"type": "file_save", "path": "cart.py", "content": base})
    check("Alice's save is accepted", await a.wait(lambda m: m.get("type") == "file_accepted") is not None)
    check("Bob, on the same branch, gets the file", (await b.wait(is_file("cart.py"))) is not None)
    got = await c.wait(is_branch_file("cart.py"))
    check("Carol, on another branch, gets it as context", got is not None and got["branch"] == "" and got["content"] == base)
    check("Carol is never sent it as a file", await c.wait(is_file("cart.py"), timeout=0.6) is None)
    check("Dave is never sent it as a file", await d.wait(is_file("cart.py"), timeout=0.6) is None)
    await d.wait(is_branch_file("cart.py"))

    # Carol's branch changes the signature; her lane has its own copy.
    taxed = base.replace("def total(self):", "def total(self, tax_rate):")
    await c.send({"type": "file_save", "path": "cart.py", "content": taxed})
    check("Carol's save is accepted in her lane", await c.wait(lambda m: m.get("type") == "file_accepted") is not None)
    check("Dave, on Carol's branch, gets the file", (await d.wait(is_file("cart.py"))) is not None)
    got = await a.wait(is_branch_file("cart.py"))
    check(
        "Alice gets Carol's change as context, with its branch and what it replaced",
        got is not None and got["branch"] == "feature/tax" and got["content"] == taxed and got["previous"] is None,
        str(got)[:200],
    )
    leaked = await a.wait(is_file("cart.py", people["carol"]["user_id"]), timeout=0.6)
    check("Alice is never sent Carol's file", leaked is None, str(leaked)[:200])
    echo = await a.wait(is_file("cart.py"), timeout=0.2)
    check("what Alice did get was her own save echoed back", echo is None or echo.get("sender") == alice["user_id"], str(echo)[:200])
    await b.wait(is_branch_file("cart.py"))

    # A patch from Dave applies to his branch's copy, not the main lane's.
    discounted = taxed + "\n    def discount(self):\n        return 0\n"
    patch = [{"k": 3}, {"i": "\n    def discount(self):\n        return 0\n"}]
    await d.send({"type": "file_patch", "path": "cart.py", "base_hash": digest(taxed), "hash": digest(discounted), "patch": patch})
    check("Dave's patch applies to his branch's copy", await d.wait(lambda m: m.get("type") == "file_accepted") is not None)
    got = await a.wait(is_branch_file("cart.py"))
    check("Alice sees Dave's change with what it replaced", got is not None and got["previous"] == taxed)

    # Catch-up comes from your own lane.
    await b.send({"type": "files_request"})
    files = await b.wait(lambda m: m.get("type") == "session_files")
    check("Bob's catch-up has the main lane's cart.py", files and [f["content"] for f in files["files"] if f["path"] == "cart.py"] == [base])
    await c.send({"type": "files_request"})
    files = await c.wait(lambda m: m.get("type") == "session_files")
    check("Carol's catch-up has her branch's cart.py", files and [f["content"] for f in files["files"] if f["path"] == "cart.py"] == [discounted])

    # Each branch keeps its own history.
    main_versions = call("GET", f"/sessions/{session}/history?path=cart.py", token=alice["token"])
    branch_versions = call("GET", f"/sessions/{session}/history?path=cart.py&branch=feature%2Ftax", token=alice["token"])
    check("main history has only Alice's version", [v["user_id"] for v in main_versions] == [alice["user_id"]])
    check("feature/tax history has Carol's and Dave's", len(branch_versions) == 2)

    # Live typing works on one shared copy: the main lane's.
    await c.send({"type": "buffer_join", "path": "cart.py"})
    refused = await c.wait(lambda m: m.get("type") == "notice")
    check("Carol can't type live into the main lane's copy", refused is not None and "another branch" in refused.get("message", ""))

    # Switching branch moves you to that lane, and everyone hears it.
    await d.send({"type": "set_branch", "branch": "main"})
    set_reply = await d.wait(lambda m: m.get("type") == "branch_set")
    check("Dave's switch is confirmed as the main lane", set_reply is not None and set_reply["main_lane"] is True)
    info = await a.wait(
        lambda m: m.get("type") == "branch_info" and m.get("user_id") == people["dave"]["user_id"] and m.get("branch") == "main"
    )
    check("Alice hears Dave changed branch", info is not None and info["branch"] == "main" and info["main_branch"] == "main")
    await a.send({"type": "file_save", "path": "notes.md", "content": "hello\n", "base_hash": None})
    check("Dave now gets main-lane files", (await d.wait(is_file("notes.md"))) is not None)
    check("Carol still gets them as context only", (await c.wait(is_branch_file("notes.md"))) is not None)

    # Carol chooses to share nothing across branches: in both directions.
    await c.send({"type": "set_branch", "branch": "feature/tax", "share_across": False})
    check("Carol's choice is confirmed", await c.wait(lambda m: m.get("type") == "branch_set") is not None)
    await a.send({"type": "file_save", "path": "notes.md", "content": "main only\n"})
    check("Carol no longer gets main's changes", await c.wait(is_branch_file("notes.md"), timeout=0.8) is None)
    await c.send({"type": "files_request"})
    await c.wait(lambda m: m.get("type") == "session_files")
    await c.send({"type": "file_save", "path": "tax.py", "content": "RATE_BP = 825\n"})
    await c.wait(lambda m: m.get("type") == "file_accepted")
    check("Alice no longer gets Carol's changes", await a.wait(is_branch_file("tax.py"), timeout=0.8) is None)
    await c.send({"type": "set_branch", "branch": "feature/tax", "share_across": True})
    await c.wait(lambda m: m.get("type") == "branch_set")
    await a.send({"type": "file_save", "path": "notes.md", "content": "shared again\n"})
    check("turning it back on shares again", (await c.wait(is_branch_file("notes.md"))) is not None)

    # A client without git (no branch) shares the main lane, as before.
    eve_account = call("POST", "/auth/register", {"email": "eve@example.test", "password": "e2e-only-password"})
    call("POST", f"/sessions/{session}/join", {"invite": invite}, eve_account["token"])
    call("PUT", f"/sessions/{session}/join-requests/{eve_account['user_id']}", {"approve": True}, alice["token"])
    e = Peer("eve", eve_account["token"], session)
    agent_runs.PORT = PORT
    check("Eve, without a branch, connects", await e.connect() is not None)
    await a.send({"type": "file_save", "path": "notes.md", "content": "hello again\n"})
    check("Eve shares the main lane", (await e.wait(is_file("notes.md"))) is not None)

    for peer in (a, b, c, d, e):
        await peer.close()


asyncio.run(main())
