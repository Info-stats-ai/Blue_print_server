"""End-to-end check of the context safeguards against a real local server,
with three people: a decision can replace an older one (and the person is
asked first), team memory only serves agent work a teammate accepted, and
anything flagged as wrong is withheld from everyone until the flag is lifted.

Run it after `cargo build`, with the websockets package (12.x) installed:
    python3 -m venv /tmp/wsenv && /tmp/wsenv/bin/pip install websockets==12.0
    /tmp/wsenv/bin/python demo/e2e/safeguards.py
"""
import asyncio, os, sys, urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agent_runs import Peer, call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

PORT = 3923
agent_runs.PORT = PORT
agent_runs.BASE = f"http://127.0.0.1:{PORT}"


def fnv(text):
    """The step key both the app and the server compute (FNV-1a, hex)."""
    value = 0xCBF29CE484222325
    for byte in text.strip().encode():
        value = ((value ^ byte) * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{value:016x}"


def is_type(kind):
    return lambda m: m.get("type") == kind


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
    alice, bob, carol = people["alice"], people["bob"], people["carol"]
    created = call("POST", "/sessions", {"name": "Safeguards", "goal": "Ship billing", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    for name in ("bob", "carol", "dave"):
        person = people[name]
        call("POST", f"/sessions/{session}/join", {"invite": invite}, person["token"])
        call("PUT", f"/sessions/{session}/join-requests/{person['user_id']}", {"approve": True}, alice["token"])

    a = Peer("alice", alice["token"], session)
    b = Peer("bob", bob["token"], session)
    c = Peer("carol", carol["token"], session)
    snapshot = await a.connect()
    await b.connect()
    await c.connect()
    check("the server says it has the safeguards", "context_safeguards" in snapshot.get("features", []), snapshot.get("features"))
    check("a new session has nothing flagged", snapshot.get("context_flags") == [])

    def memory(query, who=alice):
        return call("GET", f"/sessions/{session}/memory?q={urllib.parse.quote(query)}&limit=20", token=who["token"])

    # --- A decision replaces an older one, and the person is asked first ---
    await a.send({"type": "pin_decision", "text": "Late fees are 25 cents per day, capped at 5 dollars", "check": True})
    pinned = await b.wait(is_type("decisions"))
    check("a first decision is pinned straight away", pinned is not None and len(pinned["decisions"]) == 1)
    await a.wait(is_type("decisions"))
    await c.wait(is_type("decisions"))
    old_id = pinned["decisions"][0]["id"]

    await b.send({"type": "pin_decision", "text": "Late fees are 50 cents per day from now on", "check": True})
    asked = await b.wait(is_type("decision_check"))
    check("pinning one that may change it asks Bob first", asked is not None and [d["id"] for d in asked["related"]] == [old_id], str(asked))
    check("nothing is pinned or replaced before he answers", await a.wait(is_type("decisions"), timeout=0.6) is None)
    check("only Bob is asked", await c.wait(is_type("decision_check"), timeout=0.2) is None)

    await b.send({"type": "pin_decision", "text": asked["text"], "replaces": old_id})
    replaced = await c.wait(is_type("decisions"))
    texts = [d["text"] for d in replaced["decisions"]] if replaced else []
    check("Replace: everyone now has only the new decision", texts == ["Late fees are 50 cents per day from now on"], str(texts))
    await a.wait(is_type("decisions"))
    await b.wait(is_type("decisions"))
    hits = memory("late fees per day")
    check("team memory no longer serves the replaced decision", not any("25 cents" in hit["text"] for hit in hits), str(hits))
    check("and serves the new one, as decided by people",
          any("50 cents" in hit["text"] and hit["source"] == "decided by people" for hit in hits), str(hits))

    await c.send({"type": "pin_decision", "text": "second try", "replaces": old_id})
    error = await c.wait(is_type("error"))
    check("a decision can't be replaced twice", error is not None and "already" in error["message"], str(error))

    await c.send({"type": "pin_decision", "text": "Late fees are waived for the first 3 days", "check": True})
    asked = await c.wait(is_type("decision_check"))
    check("a related decision asks Carol too", asked is not None)
    await c.send({"type": "pin_decision", "text": asked["text"]})
    both = await a.wait(is_type("decisions"))
    check("Keep both: the two decisions stand side by side", both is not None and len(both["decisions"]) == 2, str(both))
    await b.wait(is_type("decisions"))
    await c.wait(is_type("decisions"))

    await a.send({"type": "pin_decision", "text": "Invoices are numbered INV-0001 upward", "check": True})
    unrelated = await b.wait(is_type("decisions"))
    check("an unrelated decision is pinned without asking", unrelated is not None and len(unrelated["decisions"]) == 3)
    await a.wait(is_type("decisions"))
    await c.wait(is_type("decisions"))

    # An older app sends no `check`: it still pins, as before.
    await a.send({"type": "pin_decision", "text": "Late fees are rounded to the cent"})
    old_client = await b.wait(is_type("decisions"))
    check("an older app's pin still works unchanged", old_client is not None and len(old_client["decisions"]) == 4)
    await a.wait(is_type("decisions"))
    await c.wait(is_type("decisions"))

    # --- Agent work is only remembered once a teammate accepts it ---
    claim = "Refunds are rounded down to the nearest dollar in refund_total"
    await b.send({"type": "agent_run", "run_id": "run-refund", "agent": "I-mode agent", "goal": "Add refund_total to the ledger", "status": "running", "remember": True})
    await a.wait(is_type("agent_run"))
    await b.send({"type": "agent_step", "agent": "I-mode agent", "kind": "result", "text": claim, "run_id": "run-refund", "remember": True})
    step = await a.wait(is_type("agent_step"))
    check("teammates still see the agent's step live", step is not None and step["text"] == claim)
    await b.send({"type": "agent_run", "run_id": "run-refund", "agent": "I-mode agent", "goal": "Add refund_total to the ledger", "status": "done",
                  "files": ["ledger/refunds.py"], "summary": "refund_total rounds refunds down", "remember": True})
    await a.wait(lambda m: m.get("type") == "agent_run" and m["run"]["status"] == "done")
    for peer in (b, c):
        while await peer.wait(lambda m: m.get("type") in ("agent_run", "agent_step"), timeout=0.2):
            pass

    hits = memory("refund_total rounds refunds")
    check("unreviewed agent work isn't in team memory", not any("refund" in hit["text"].lower() for hit in hits), str(hits))

    await a.send({"type": "agent_run_review", "user_id": bob["user_id"], "run_id": "run-refund", "verdict": "accepted"})
    await b.wait(lambda m: m.get("type") == "agent_run" and m["run"]["reviews"])
    hits = memory("refund_total rounds refunds")
    run_hit = next((hit for hit in hits if hit["kind"] == "run"), None)
    check("once Alice accepts it, memory serves the run", run_hit is not None and run_hit["source"] == "agent work a teammate accepted", str(hits))
    check("and its steps, still marked as an agent's claim",
          any(hit["kind"] == "step" and claim in hit["text"] and hit["source"] == "an agent said, unchecked" for hit in hits), str(hits))

    # --- Flag as wrong: withdrawn for everyone, until undone ---
    run_key = f"run:{bob['user_id']}:run-refund"
    await c.send({"type": "flag_context", "key": run_key, "note": "Refunds must round to the cent, not the dollar"})
    for peer in (a, b, c):
        told = await peer.wait(is_type("context_flags"))
        check(f"{peer.name} is told the run was flagged",
              told is not None and told["flags"][0]["key"] == run_key and told["flags"][0]["flagged_by"] == carol["user_id"]
              and "round to the cent" in told["flags"][0]["note"], str(told))
    hits = memory("refund_total rounds refunds")
    check("a flagged run and its steps leave team memory", not any("refund" in hit["text"].lower() for hit in hits), str(hits))

    d = Peer("dave", people["dave"]["token"], session)
    late = await d.connect()
    check("someone joining later gets the flags too", [flag["key"] for flag in late.get("context_flags", [])] == [run_key], str(late.get("context_flags")))
    for peer in (a, b, c):
        while await peer.wait(lambda m: m.get("type") in ("peer_joined", "presence", "branch_info"), timeout=0.2):
            pass

    await a.send({"type": "unflag_context", "key": run_key})
    lifted = await d.wait(is_type("context_flags"))
    check("Undo lifts the flag for everyone", lifted is not None and lifted["flags"] == [], str(lifted))
    for peer in (a, b, c):
        await peer.wait(is_type("context_flags"))
    check("and the accepted run is served again", any(hit["kind"] == "run" for hit in memory("refund_total rounds refunds")))

    # A loose step (no run), flagged by its text key.
    loose = "The API returns amounts in dollars as floats"
    await b.send({"type": "agent_step", "agent": "Agent panel", "kind": "result", "text": loose, "remember": True})
    await a.wait(is_type("agent_step"))
    check("a loose remembered step is in memory, marked unchecked",
          any(loose in hit["text"] and hit["source"] == "an agent said, unchecked" for hit in memory("API amounts dollars floats")))
    await a.send({"type": "flag_context", "key": f"step:{bob['user_id']}:{fnv(loose)}", "note": "amounts are integer cents"})
    await a.wait(is_type("context_flags"))
    check("flagging that step withdraws it", not any(loose in hit["text"] for hit in memory("API amounts dollars floats")))

    # Sent back: never served, whatever else happened.
    await c.send({"type": "agent_run_review", "user_id": bob["user_id"], "run_id": "run-refund", "verdict": "changes", "note": "round to the cent"})
    await b.wait(lambda m: m.get("type") == "agent_run" and len(m["run"]["reviews"]) == 2)
    check("a run someone sent back leaves memory even though another accepted it",
          not any(hit["kind"] == "run" for hit in memory("refund_total rounds refunds")))

    # --- The summary: flagged means withheld until it's rewritten ---
    for peer in (a, b, c, d):
        while await peer.wait(is_type("context_flags"), timeout=0.2):
            pass
    await a.send({"type": "flag_context", "key": "digest", "note": "we never agreed on Stripe"})
    told = await b.wait(is_type("context_flags"))
    check("flagging the summary reaches everyone", told is not None and any(flag["key"] == "digest" for flag in told["flags"]), str(told))
    # This server has no summary yet, so there's nothing to rewrite: the
    # flag is lifted on its own instead of blocking a summary that isn't there.
    cleared = await b.wait(lambda m: m.get("type") == "context_flags" and not any(flag["key"] == "digest" for flag in m["flags"]))
    check("with no summary to rewrite, the flag clears itself", cleared is not None, str(cleared))

    # --- Bad input ---
    await a.send({"type": "flag_context", "key": "decision:1"})
    error = await a.wait(is_type("error"))
    check("only runs, steps and the summary can be flagged", error is not None and "can't be flagged" in error["message"], str(error))
    await a.send({"type": "flag_context", "key": "run:x'; DROP TABLE decisions;--"})
    check("a malformed key is refused", await a.wait(is_type("error")) is not None)
    check("and decisions are untouched", len(call("GET", f"/sessions/{session}/memory?q=late%20fees&limit=20", token=alice["token"])) > 0)

    outsider = call("POST", "/auth/register", {"email": "mallory@example.test", "password": "e2e-only-password"})
    try:
        call("GET", f"/sessions/{session}/memory?q=fees", token=outsider["token"])
        check("someone outside the session can't search its memory", False)
    except Exception as error:
        check("someone outside the session can't search its memory", "40" in str(error), str(error))

    for peer in (a, b, c, d):
        await peer.close()


asyncio.run(main())
