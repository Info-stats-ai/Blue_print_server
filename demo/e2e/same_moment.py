"""End-to-end check of two people changing the same file at the same moment,
for a file the session has never had: neither change may quietly replace the
other. The second person's starting point (the committed version) must not
reset the server's copy over the first person's change, and their own change
must be refused as out of date, so their app merges instead of overwriting.

Run it after `cargo build`, with Python 3.14+ and websockets 12:
    /tmp/wsenv/bin/python demo/e2e/same_moment.py
"""
import asyncio, hashlib, os, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import catchup  # noqa: E402
from agent_runs import call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

catchup.PORT = 3925
catchup.STATE = catchup.tempfile.mkdtemp(prefix="same-moment-")
agent_runs.PORT = catchup.PORT
agent_runs.BASE = f"http://127.0.0.1:{catchup.PORT}"


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


def one_line_patch(old, new):
    """Line operations turning `old` into `new` (they differ in one line)."""
    a, b = old.splitlines(keepends=True), new.splitlines(keepends=True)
    at = next(i for i, (x, y) in enumerate(zip(a, b)) if x != y)
    ops = []
    if at:
        ops.append({"k": at})
    ops += [{"d": 1}, {"i": b[at]}]
    if len(a) - at - 1:
        ops.append({"k": len(a) - at - 1})
    return ops


async def main():
    server = catchup.start_server()
    people = {n: call("POST", "/auth/register", {"email": f"{n}@example.test", "password": "e2e-only-password"}) for n in ("alice", "bob")}
    alice, bob = people["alice"], people["bob"]
    created = call("POST", "/sessions", {"name": "Same moment", "goal": "Lose nothing", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    call("POST", f"/sessions/{session}/join", {"invite": invite}, bob["token"])
    call("PUT", f"/sessions/{session}/join-requests/{bob['user_id']}", {"approve": True}, alice["token"])
    a = catchup.Client("alice", alice["token"], session)
    b = catchup.Client("bob", bob["token"], session)
    await a.connect()
    await b.connect()

    path = "shop/returns.py"
    committed = "def refund_for(order):\n    return 0\n"
    bobs = "def refund_for(order):  # BOB\n    return 0\n"
    alices = "def refund_for(order):  # ALICE\n    return 0\n"

    # Bob gets there first: his starting point, then his change.
    await b.send({"type": "file_save", "path": path, "content": committed, "origin": "share"})
    await b.send({"type": "file_patch", "path": path, "base_hash": sha(committed), "hash": sha(bobs), "patch": one_line_patch(committed, bobs)})
    accepted = await b.wait(lambda m: m.get("type") == "file_accepted" and m.get("hash") == sha(bobs))
    check("Bob's change is accepted", accepted is not None)
    await a.wait(lambda m: m.get("type") == "file_patched" and m.get("path") == path)

    # Alice's app, a moment behind, sends its own starting point and change.
    a.inbox.clear()
    b.inbox.clear()
    await a.send({"type": "file_save", "path": path, "content": committed, "origin": "share"})
    await a.send({"type": "file_patch", "path": path, "base_hash": sha(committed), "hash": sha(alices), "patch": one_line_patch(committed, alices)})
    resend = await a.wait(lambda m: m.get("type") == "file_resend" and m.get("path") == path)
    check("Alice's change is refused as out of date, so her app merges", resend is not None and resend.get("known") is True, resend)
    check("Alice's late starting point reaches no one", await b.wait(lambda m: m.get("path") == path and m.get("origin") == "share", timeout=1.0) is None)

    # The team's copy is still Bob's change, not the commit and not Alice's.
    await a.send({"type": "file_fetch", "path": path})
    fetched = await a.wait(lambda m: m.get("type") == "file_saved" and m.get("path") == path)
    check("The team's copy is still Bob's change", fetched is not None and fetched.get("content") == bobs, fetched and fetched.get("content"))

    # A starting point still counts for a file nobody has shared.
    fresh = "VERSION = 1\n"
    await a.send({"type": "file_save", "path": "shop/version.py", "content": fresh, "origin": "share"})
    got = await b.wait(lambda m: m.get("path") == "shop/version.py")
    check("A first starting point is still shared", got is not None and got.get("content") == fresh)

    for peer in (a, b):
        await peer.close()
    server.terminate()
    print("\nALL PASSED" if not failures else f"\n{len(failures)} FAILED")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    asyncio.run(main())
