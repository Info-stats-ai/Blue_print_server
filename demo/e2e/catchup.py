"""End-to-end check of catching up on a session's files against a real local
server: one fingerprint answers "up to date", otherwise only differing files
travel, as recipes that reuse the chunks the client already has, each checked
by SHA-256. Large messages go zstd-compressed both ways.

Run it after `cargo build`, with Python 3.14+ (for compression.zstd) and the
websockets package (12.x):
    /tmp/wsenv/bin/python demo/e2e/catchup.py
"""
import asyncio, hashlib, json, os, subprocess, sys, tempfile, time, urllib.request
import compression.zstd as zstd
import websockets

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agent_runs import Peer, call, check, failures  # noqa: E402
import agent_runs  # noqa: E402

SERVER = os.path.expanduser("~/blueprint-server/target/debug/blueprint-server")
PORT = 3924
STATE = tempfile.mkdtemp(prefix="catchup-")
agent_runs.PORT = PORT
agent_runs.BASE = f"http://127.0.0.1:{PORT}"


def start_server():
    env = dict(os.environ, DATABASE_URL=f"sqlite:{STATE}/e2e.db", JWT_SECRET="e2e-local-only", PORT=str(PORT))
    env.pop("ANTHROPIC_API_KEY", None)
    proc = subprocess.Popen([SERVER], env=env, cwd=STATE, stdout=open(f"{STATE}/server.log", "a"), stderr=subprocess.STDOUT)
    for _ in range(80):
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/robots.txt", timeout=1)
            return proc
        except Exception:
            time.sleep(0.25)
    raise SystemExit("server didn't start; see " + STATE)


class OldClient(Peer):
    """An app from before compressed frames and catch-up."""

    async def connect(self):
        self.ws = await websockets.connect(
            f"ws://127.0.0.1:{PORT}/ws?session_id={self.session}&features=file_patch",
            extra_headers={"authorization": f"Bearer {self.token}"},
            max_size=None,
        )
        self.reader = asyncio.create_task(self._read())
        return await self.wait(lambda m: m.get("type") == "snapshot")

# The chunker, a third copy (the server and the app have the others): the
# same text must cut at the same places for chunks to be reused.
M64 = (1 << 64) - 1
MIN, AVG, MAX = 1024, 4096, 32 * 1024
HARD, EASY = (M64 << (64 - 14)) & M64, (M64 << (64 - 10)) & M64


def _gear():
    state, table = 0x1D0EC0DE5EED2026, []
    for _ in range(256):
        state = (state + 0x9E3779B97F4A7C15) & M64
        z = state
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & M64
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & M64
        table.append(z ^ (z >> 31))
    return table


GEAR = _gear()


def chunks(text):
    data, start, out = text.encode(), 0, []
    while start < len(data):
        n = len(data) - start
        cut = n
        if n > MIN:
            limit, h, want, cut = min(n, MAX), 0, False, None
            for off in range(MIN, limit):
                b = data[start + off]
                if not want:
                    h = ((h << 1) + GEAR[b]) & M64
                    want = (h & (HARD if off < AVG else EASY)) == 0
                if want and b == 10:
                    cut = off + 1
                    break
            if cut is None:
                if limit == n:
                    cut = n
                else:
                    p = data[start + MIN:start + limit].rfind(b"\n")
                    if p >= 0:
                        cut = MIN + p + 1
                    else:
                        cut = limit
                        while cut > MIN and data[start + cut] & 0xC0 == 0x80:
                            cut -= 1
        out.append(data[start:start + cut].decode())
        start += cut
    return out


def sha(text):
    return hashlib.sha256(text.encode()).hexdigest()


class Client(Peer):
    """The new app: compressed frames and catch-up."""

    async def connect(self):
        self.frames = []
        self.ws = await websockets.connect(
            f"ws://127.0.0.1:{PORT}/ws?session_id={self.session}&features=file_patch,zstd_frames",
            extra_headers={"authorization": f"Bearer {self.token}"},
            max_size=None,
        )
        self.reader = asyncio.create_task(self._read())
        return await self.wait(lambda m: m.get("type") == "snapshot")

    async def _read(self):
        try:
            async for raw in self.ws:
                if isinstance(raw, bytes):
                    self.frames.append(len(raw))
                    raw = zstd.decompress(raw).decode()
                self.inbox.append(json.loads(raw))
        except Exception:
            pass

    async def send_compressed(self, message):
        await self.ws.send(zstd.compress(json.dumps(message).encode()))


def rebuild(parts, have):
    return "".join(have[part["c"]] if "c" in part else part["t"] for part in parts)


async def main():
    server = start_server()
    people = {n: call("POST", "/auth/register", {"email": f"{n}@example.test", "password": "e2e-only-password"}) for n in ("alice", "bob")}
    alice, bob = people["alice"], people["bob"]
    created = call("POST", "/sessions", {"name": "Catch-up", "goal": "Send only what differs", "has_leader": True}, alice["token"])
    session, invite = created["session"]["id"], created["invite_token"]
    call("POST", f"/sessions/{session}/join", {"invite": invite}, bob["token"])
    call("PUT", f"/sessions/{session}/join-requests/{bob['user_id']}", {"approve": True}, alice["token"])

    a = Client("alice", alice["token"], session)
    snapshot = await a.connect()
    check("The server offers catch-up and compressed frames", {"catch_up", "zstd_frames"} <= set((snapshot or {}).get("features", [])))

    big = "".join(f"def handler_{n}(request):\n    return respond(request, code={n % 7}, body='item {n}')\n\n" for n in range(3000))
    small = "def total(cart):\n    return sum(item.price for item in cart)\n"
    same = "VERSION = '1.0'\n"
    for path, content in (("app/big.py", big), ("app/cart.py", small), ("app/version.py", same)):
        await a.send({"type": "file_save", "path": path, "content": content})
    await asyncio.sleep(0.5)

    # Bob has an older big.py (one function changed since), the same
    # version.py, and no cart.py.
    bob_big = big.replace("def handler_1500(request):", "def handler_1500(req):", 1)
    bob_files = {"app/big.py": bob_big, "app/version.py": same}

    b = Client("bob", bob["token"], session)
    await b.connect()
    await b.send({"type": "catch_up_request"})
    manifest = await b.wait(lambda m: m.get("type") == "catch_up_manifest")
    entries = {entry["path"]: entry for entry in (manifest or {}).get("files", [])}
    check("With no fingerprint, the list of files comes back", manifest is not None and set(entries) == {"app/big.py", "app/cart.py", "app/version.py"}, manifest)
    check("Each file is listed with its SHA-256 and size", entries.get("app/big.py", {}).get("hash") == sha(big) and entries["app/big.py"]["size"] == len(big.encode()))

    wants, used_have = [], {}
    for path, entry in entries.items():
        mine = bob_files.get(path)
        if mine is not None and sha(mine) == entry["hash"]:
            continue
        have = chunks(mine) if mine is not None else []
        used_have[path] = have
        wants.append({"path": path, "have": [blake3_name(c) for c in have]})
    check("Only the files that differ are asked for", sorted(w["path"] for w in wants) == ["app/big.py", "app/cart.py"])
    await b.send_compressed({"type": "catch_up_fetch", "files": wants})
    reply = await b.wait(lambda m: m.get("type") == "catch_up_files")
    got = {f["path"]: f for f in (reply or {}).get("files", [])}
    rebuilt = {path: rebuild(f["parts"], used_have[path]) for path, f in got.items()}
    check("Every file rebuilds to exactly the team's version", rebuilt.get("app/big.py") == big and rebuilt.get("app/cart.py") == small)
    check("Each is checked by its SHA-256", all(sha(rebuilt[p]) == got[p]["hash"] for p in got))
    sent_text = sum(len(part.get("t", "")) for part in got.get("app/big.py", {}).get("parts", []))
    check(f"For a one-line difference, a sliver of big.py travels ({sent_text} of {len(big)} bytes)", 0 < sent_text < len(big) // 20)
    check("A file the client doesn't have comes whole", got.get("app/cart.py", {}).get("parts") == [{"t": small}])

    # Now up to date: one fingerprint, one small answer.
    for path, content in rebuilt.items():
        bob_files[path] = content
    root = blake3_root({p: sha(c) for p, c in bob_files.items()})
    await b.send({"type": "catch_up_request", "root": root})
    up = await b.wait(lambda m: m.get("type") == "catch_up_manifest")
    check("With a matching fingerprint the answer is just 'up to date'", up is not None and up.get("up_to_date") is True and up.get("files") == [], up)

    # A large message arrives compressed.
    b.frames.clear()
    await b.send({"type": "catch_up_request"})
    await b.wait(lambda m: m.get("type") == "catch_up_manifest")
    huge = big * 3
    await a.send({"type": "file_save", "path": "app/huge.py", "content": huge})
    saved = await b.wait(lambda m: m.get("type") == "file_saved" and m.get("path") == "app/huge.py", timeout=5)
    check("A large file arrives in a compressed frame", saved is not None and saved["content"] == huge and any(size < len(huge) // 4 for size in b.frames), b.frames[-3:])

    # An older client still gets plain text.
    old = OldClient("bob-old", bob["token"], session)
    await old.connect()
    await old.send({"type": "files_request"})
    plain = await old.wait(lambda m: m.get("type") == "session_files", timeout=5)
    check("A client without compressed frames still gets plain text", plain is not None and len(plain.get("files", [])) == 4)

    for peer in (a, b, old):
        await peer.close()
    server.terminate()
    print("\nALL PASSED" if not failures else f"\n{len(failures)} FAILED")
    sys.exit(1 if failures else 0)


def blake3_name(chunk):
    return _blake3(chunk.encode())[:32]


def blake3_root(pairs):
    data = b"".join(p.encode() + b"\0" + h.encode() + b"\n" for p, h in sorted(pairs.items()))
    return _blake3(data)


def _blake3(data):
    # BLAKE3 isn't in Python's standard library: `cargo build --example b3`.
    tool = os.path.expanduser("~/blueprint-server/target/debug/examples/b3")
    out = subprocess.run([tool], input=data, capture_output=True, check=True)
    return out.stdout.decode().strip()


if __name__ == "__main__":
    asyncio.run(main())
