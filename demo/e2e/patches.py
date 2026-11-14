"""End-to-end check of sending files as patches against a real local server:
a small change travels as the lines that changed, clients that can't apply
patches still get the whole file, and anything that doesn't fit falls back to
a full send instead of producing a wrong file.

Run it after `cargo build`, with the websockets package (12.x) installed:
    python3 -m venv /tmp/wsenv && /tmp/wsenv/bin/pip install websockets==12.0
    /tmp/wsenv/bin/python demo/e2e/patches.py
"""
import asyncio, difflib, hashlib, json, os, subprocess, sys, tempfile, time, urllib.request
import websockets

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agent_runs import Peer, call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

SERVER = os.path.expanduser("~/blueprint-server/target/debug/blueprint-server")
PORT = 3920
STATE = tempfile.mkdtemp(prefix="patch-")
agent_runs.PORT = PORT
agent_runs.BASE = f"http://127.0.0.1:{PORT}"
KEY = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx"


class PatchPeer(Peer):
    """A client that says it applies patches, like the current app."""

    async def connect(self):
        self.ws = await websockets.connect(
            f"ws://127.0.0.1:{PORT}/ws?session_id={self.session}&features=file_patch",
            extra_headers={"authorization": f"Bearer {self.token}"},
            max_size=None,
        )
        self.reader = asyncio.create_task(self._read())
        return await self.wait(lambda m: m.get("type") == "snapshot")


def start_server():
    env = dict(os.environ, DATABASE_URL=f"sqlite:{STATE}/e2e.db", JWT_SECRET="e2e-local-only", PORT=str(PORT))
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


def digest(text):
    return hashlib.sha256(text.encode()).hexdigest()


def diff(old, new):
    """Line operations turning `old` into `new`, in the wire form."""
    a, b = old.splitlines(keepends=True), new.splitlines(keepends=True)
    ops = []
    for tag, i1, i2, j1, j2 in difflib.SequenceMatcher(None, a, b, autojunk=False).get_opcodes():
        if tag == "equal":
            ops.append({"k": i2 - i1})
            continue
        if i2 > i1:
            ops.append({"d": i2 - i1})
        if j2 > j1:
            ops.append({"i": "".join(b[j1:j2])})
    return ops


def patch_message(path, old, new, **extra):
    return {"type": "file_patch", "path": path, "base_hash": digest(old), "hash": digest(new), "patch": diff(old, new), **extra}


def of(kind, path):
    return lambda m: m.get("type") == kind and m.get("path") == path


async def main():
    server = start_server()
    people = {}
    for name in ("alice", "bob", "carol"):
        people[name] = call("POST", "/auth/register", {"email": f"{name}@example.test", "password": "e2e-only-password"})
    alice, bob, carol = people["alice"], people["bob"], people["carol"]
    created = call("POST", "/sessions", {"name": "Patches", "goal": "Send only what changed", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    for person in (bob, carol):
        call("POST", f"/sessions/{session}/join", {"invite": invite}, person["token"])
        call("PUT", f"/sessions/{session}/join-requests/{person['user_id']}", {"approve": True}, alice["token"])

    a = PatchPeer("alice", alice["token"], session)
    b = PatchPeer("bob", bob["token"], session)
    c = Peer("carol", carol["token"], session)  # an older app: whole files only
    snapshot = await a.connect()
    await b.connect()
    await c.connect()
    check("The server says it takes patches", "file_patch" in (snapshot or {}).get("features", []), (snapshot or {}).get("features"))

    path = "shoplite/big.py"
    v1 = "".join(f"def f{n}():\n    return {n}\n\n" for n in range(400))
    await a.send({"type": "file_save", "path": path, "content": v1})
    first = await b.wait(of("file_saved", path))
    check("A new file arrives whole", first is not None and first["content"] == v1)
    await c.wait(of("file_saved", path))

    # A one-line change travels as a patch to Bob and whole to Carol.
    v2 = v1.replace("    return 200\n", "    return 200 + 1\n")
    b.inbox.clear(); c.inbox.clear()
    await a.send(patch_message(path, v1, v2, origin="agent"))
    patched = await b.wait(of("file_patched", path))
    whole = await c.wait(of("file_saved", path))
    check("Bob gets the change as a patch", patched is not None and patched["base_hash"] == digest(v1)
          and patched["hash"] == digest(v2) and patched["sender"] == alice["user_id"] and patched.get("origin") == "agent", patched)
    check("Bob isn't also sent the whole file", not any(of("file_saved", path)(m) for m in b.inbox))
    check("An older app gets the whole file", whole is not None and whole["content"] == v2 and whole["sender"] == alice["user_id"])
    if patched and whole:
        small, big = len(json.dumps(patched)), len(json.dumps(whole))
        print(f"     patch {small} bytes, whole file {big} bytes ({big / small:.0f}x smaller)")
        check("The patch is a small fraction of the file", small * 20 < big, (small, big))

    # Asking for the whole file returns the server's copy, with who changed it.
    await b.send({"type": "file_fetch", "path": path, "sender": alice["user_id"], "origin": "agent"})
    fetched = await b.wait(of("file_saved", path))
    check("A client can ask for the whole file", fetched is not None and fetched["content"] == v2
          and fetched["sender"] == alice["user_id"] and fetched.get("origin") == "agent", fetched and fetched.get("sender"))
    check("Only the asker gets it", await c.wait(of("file_saved", path), timeout=0.5) is None)

    # History holds the patched version in full.
    versions = call("GET", f"/sessions/{session}/history?path={path}", token=bob["token"])
    newest = call("GET", f"/sessions/{session}/history/{versions[0]['id']}", token=bob["token"])
    check("History keeps the patched version whole", len(versions) == 2 and newest["content"] == v2
          and versions[0]["origin"] == "agent" and (versions[0]["added"], versions[0]["removed"]) == (1, 1), versions[0])

    # Patches that don't fit are never applied: the sender is asked to resend.
    v3 = v2.replace("    return 300\n", "    return 300 + 1\n")
    for label, message in [
        ("made from an older version", patch_message(path, v1, v3)),
        ("with the wrong result hash", {**patch_message(path, v2, v3), "hash": digest("something else")}),
        ("that runs past the end", {**patch_message(path, v2, v3), "patch": [{"k": 5000}]}),
        ("for a file the team doesn't have", patch_message("shoplite/missing.py", "", "x = 1\n")),
    ]:
        b.inbox.clear(); a.inbox.clear()
        await a.send(message)
        resend = await a.wait(lambda m: m.get("type") == "file_resend" and m.get("path") == message["path"])
        check(f"A patch {label} asks for a resend", resend is not None, a.inbox[-3:])
        check(f"A patch {label} reaches no one", await b.wait(lambda m: m.get("type") in ("file_patched", "file_saved"), timeout=0.4) is None)
    await b.send({"type": "file_fetch", "path": path})
    check("The team's copy is untouched by bad patches", (await b.wait(of("file_saved", path)))["content"] == v2)

    # A patch that adds a key is refused like a whole file would be.
    secret = v2 + f"KEY = '{KEY}'\n"
    a.inbox.clear(); b.inbox.clear()
    await a.send(patch_message(path, v2, secret))
    notice = await a.wait(lambda m: m.get("type") == "notice")
    check("A patch that adds a key isn't shared", notice is not None and "looks like it contains a key" in notice["message"], notice)
    check("…and never reaches Bob", await b.wait(lambda m: m.get("type") in ("file_patched", "file_saved"), timeout=0.5) is None)

    # After a full resend, patches flow again from the new version.
    await a.send({"type": "file_save", "path": path, "content": v3})
    resent = await b.wait(of("file_saved", path))
    check("A full resend goes to everyone whole", resent is not None and resent["content"] == v3)
    v4 = v3 + "def last():\n    return None\n"
    await a.send(patch_message(path, v3, v4))
    again = await b.wait(of("file_patched", path))
    check("Patches work again afterwards", again is not None and again["hash"] == digest(v4), again)

    for peer in (a, b, c):
        await peer.close()
    server.terminate()
    server.wait()
    print()
    print("ALL PASSED" if not failures else f"{len(failures)} FAILED: {', '.join(failures)}")
    sys.exit(1 if failures else 0)


asyncio.run(main())
