"""Cost/latency benchmark for I's multi-user features against a local
blueprint-server. Standard library only.

    python3 bench/bench.py <section> --base http://127.0.0.1:3919 --db <server sqlite path> --secret <JWT_SECRET>

Sections: broadcast, collisions, preview, listen, prod. Each prints one JSON
object with its results.

Users and sessions are written straight into the server's SQLite database and
tokens are minted with the server's JWT secret, so the auth rate limit (10
logins per burst per IP) doesn't throttle 25 simulated users.
"""

import argparse
import base64
import concurrent.futures
import hashlib
import hmac
import http.client
import json
import os
import random
import socket
import sqlite3
import statistics
import struct
import sys
import threading
import time
import urllib.request
import uuid

NOW = lambda: time.perf_counter()


# ---------------------------------------------------------------- setup ----

def b64url(data: bytes) -> str:
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def mint_jwt(user_id: str, secret: str) -> str:
    header = b64url(json.dumps({"alg": "HS256", "typ": "JWT"}).encode())
    payload = b64url(json.dumps({"sub": user_id, "exp": int(time.time()) + 86400}).encode())
    signature = hmac.new(secret.encode(), f"{header}.{payload}".encode(), hashlib.sha256).digest()
    return f"{header}.{payload}.{b64url(signature)}"


class Fixture:
    def __init__(self, db_path: str, secret: str):
        self.db_path = db_path
        self.secret = secret

    def session(self, people: int, name: str, listens: bool = False):
        """Creates `people` users in one new session; returns (session_id, [(user_id, name, token)])."""
        now = "2026-01-01T00:00:00Z"
        con = sqlite3.connect(self.db_path, timeout=30)
        users = []
        tag = uuid.uuid4().hex[:6]
        for i in range(people):
            user_id = str(uuid.uuid4())
            handle = f"u{i}-{tag}"
            con.execute(
                "INSERT INTO users (id, email, password_hash, created_at) VALUES (?, ?, 'x', ?)",
                (user_id, f"{handle}@bench.test", now),
            )
            users.append((user_id, handle, mint_jwt(user_id, self.secret)))
        session_id = str(uuid.uuid4())
        con.execute(
            "INSERT INTO sessions (id, name, owner_id, created_at, agent_listens) VALUES (?, ?, ?, ?, ?)",
            (session_id, name, users[0][0], now, 1 if listens else 0),
        )
        for user_id, _, _ in users:
            con.execute(
                "INSERT INTO session_members (session_id, user_id, role, joined_at) VALUES (?, ?, 'member', ?)",
                (session_id, user_id, now),
            )
        con.commit()
        con.close()
        return session_id, users


# ------------------------------------------------------------ websocket ----

class Client:
    """A raw websocket client; `on_message(client, message, received_at)` runs on its reader thread."""

    def __init__(self, base: str, session_id: str, token: str, user_id: str, on_message):
        host_port = base.split("://", 1)[1]
        host, port = host_port.split(":")
        self.user_id = user_id
        self.on_message = on_message
        self.lock = threading.Lock()
        self.snapshot = threading.Event()
        self.closed = False
        self.sock = socket.create_connection((host, int(port)))
        self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(
            (
                f"GET /ws?session_id={session_id}&token={token} HTTP/1.1\r\nHost: {host_port}\r\n"
                f"Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                "Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        response = b""
        while b"\r\n\r\n" not in response:
            response += self.sock.recv(1)
        if b" 101 " not in response.split(b"\r\n")[0]:
            raise RuntimeError(response.split(b"\r\n")[0])
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def send(self, value):
        payload = json.dumps(value).encode()
        mask = os.urandom(4)
        header = bytes([0x81])
        length = len(payload)
        if length < 126:
            header += bytes([0x80 | length])
        elif length < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", length)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", length)
        frame = header + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        with self.lock:
            self.sock.sendall(frame)

    def _pong(self, payload):
        mask = os.urandom(4)
        frame = bytes([0x8A, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        with self.lock:
            self.sock.sendall(frame)

    def _read(self):
        buffer = b""
        try:
            while not self.closed:
                chunk = self.sock.recv(1 << 16)
                if not chunk:
                    return
                buffer += chunk
                while len(buffer) >= 2:
                    opcode, length, offset = buffer[0] & 0x0F, buffer[1] & 0x7F, 2
                    if length == 126:
                        if len(buffer) < 4:
                            break
                        length, offset = struct.unpack(">H", buffer[2:4])[0], 4
                    elif length == 127:
                        if len(buffer) < 10:
                            break
                        length, offset = struct.unpack(">Q", buffer[2:10])[0], 10
                    if len(buffer) < offset + length:
                        break
                    payload, buffer = buffer[offset:offset + length], buffer[offset + length:]
                    received_at = NOW()
                    if opcode == 0x9:
                        self._pong(payload)
                    elif opcode == 0x1:
                        message = json.loads(payload)
                        if message.get("type") == "snapshot":
                            self.snapshot.set()
                        self.on_message(self, message, received_at)
        except OSError:
            return

    def close(self):
        self.closed = True
        try:
            self.sock.close()
        except OSError:
            pass


def percentiles(samples_ms):
    if not samples_ms:
        return {}
    ordered = sorted(samples_ms)
    pick = lambda q: ordered[min(len(ordered) - 1, int(round(q * (len(ordered) - 1))))]
    return {
        "n": len(ordered),
        "p50_ms": round(pick(0.50), 2),
        "p95_ms": round(pick(0.95), 2),
        "max_ms": round(ordered[-1], 2),
    }


def connect(args, session_id, token, user_id, on_message):
    # The server rate-limits per IP (burst 100, then 1 per 600 ms). Every
    # simulated user here shares one IP, unlike a real team, so wait it out.
    for _ in range(200):
        try:
            return Client(args.base, session_id, token, user_id, on_message)
        except RuntimeError as error:
            if b"429" not in error.args[0]:
                raise
            time.sleep(0.65)
    raise RuntimeError("rate limited for too long")


def connect_all(args, session_id, users, on_message):
    clients = [connect(args, session_id, token, user_id, on_message) for user_id, _, token in users]
    for client in clients:
        if not client.snapshot.wait(10):
            raise RuntimeError("no snapshot")
    time.sleep(0.5)  # let peer_joined broadcasts settle
    return clients


# ------------------------------------------------------------ sections -----

def broadcast(args):
    results = {}
    for people in (5, 10, 25):
        session_id, users = Fixture(args.db, args.secret).session(people, f"bench-broadcast-{people}")
        sent = {}
        lock = threading.Lock()
        chat_ms, cursor_ms, claim_ms = [], [], []
        delivered = [0]
        claim_waiters = {}

        def on_message(client, message, received_at):
            kind = message.get("type")
            if kind == "chat" and message.get("text", "").startswith("bench|"):
                _, sender, seq = message["text"].split("|")[:3]
                if sender != client.user_id:
                    with lock:
                        chat_ms.append((received_at - sent[("chat", sender, int(seq))]) * 1000)
                        delivered[0] += 1
            elif kind == "cursor_moved" and message.get("path", "").startswith("bench/"):
                _, sender, seq = message["path"].split("/")
                if sender != client.user_id:
                    with lock:
                        cursor_ms.append((received_at - sent[("cursor", sender, int(seq))]) * 1000)
                        delivered[0] += 1
            elif kind == "claims":
                waiter = claim_waiters.get(client.user_id)
                if waiter and any(c.get("path") == waiter[0] for c in message.get("claims", [])):
                    claim_ms.append((received_at - waiter[1]) * 1000)
                    claim_waiters.pop(client.user_id, None)

        clients = connect_all(args, session_id, users, on_message)
        per_user = 20

        def run(client):
            for seq in range(per_user):
                with lock:
                    sent[("chat", client.user_id, seq)] = NOW()
                client.send({"type": "chat", "text": f"bench|{client.user_id}|{seq}|x"})
                with lock:
                    sent[("cursor", client.user_id, seq)] = NOW()
                client.send({"type": "cursor_moved", "path": f"bench/{client.user_id}/{seq}", "row": seq})
                time.sleep(0.05)

        expected = people * per_user * 2 * (people - 1)
        started = NOW()
        threads = [threading.Thread(target=run, args=(client,)) for client in clients]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        deadline = time.time() + 20
        while delivered[0] < expected and time.time() < deadline:
            time.sleep(0.05)
        elapsed = NOW() - started

        for client in clients:
            path = f"bench/claim-{client.user_id}.py"
            claim_waiters[client.user_id] = (path, NOW())
            client.send({"type": "set_claims", "claims": [{"path": path, "holder": "I-mode agent", "symbols": ["f"]}]})
            end = time.time() + 5
            while client.user_id in claim_waiters and time.time() < end:
                time.sleep(0.002)

        for client in clients:
            client.close()
        results[str(people)] = {
            "messages_sent": people * per_user * 2,
            "deliveries_expected": expected,
            "deliveries_received": delivered[0],
            "deliveries_per_sec": round(delivered[0] / elapsed, 1),
            "chat": percentiles(chat_ms),
            "cursor": percentiles(cursor_ms),
            "claim_round_trip": percentiles(claim_ms),
        }
    return results


def collisions(args):
    results = {}
    files, functions, trials = 4, 10, 5
    for agents in (5, 10, 25):
        totals = {"claims": 0, "same_function_overlaps": 0, "refused": 0, "correctly_refused": 0,
                  "wrongly_refused": 0, "accepted_function_level": 0, "accepted_file_level": 0}
        for trial in range(trials):
            rng = random.Random(1000 * agents + trial)
            session_id, users = Fixture(args.db, args.secret).session(agents, f"bench-collisions-{agents}-{trial}")
            latest = {"claims": None}
            updated = threading.Event()

            def on_message(client, message, _received_at):
                if message.get("type") == "claims" and client is observer[0]:
                    latest["claims"] = message.get("claims", [])
                    updated.set()

            observer = [None]
            clients = connect_all(args, session_id, users, on_message)
            observer[0] = clients[0]
            held = set()
            held_files = set()
            for client in clients:
                target = (f"src/module_{rng.randrange(files)}.py", f"fn_{rng.randrange(functions)}")
                overlaps = target in held
                updated.clear()
                client.send({"type": "set_claims", "claims": [
                    {"path": target[0], "holder": "I-mode agent", "symbols": [target[1]]}]})
                if not updated.wait(5):
                    raise RuntimeError("no claims update")
                mine = [c for c in latest["claims"] if c.get("user_id") == client.user_id]
                accepted = bool(mine)
                totals["claims"] += 1
                totals["same_function_overlaps"] += overlaps
                if accepted:
                    held.add(target)
                    totals["accepted_function_level"] += 1
                else:
                    totals["refused"] += 1
                    totals["correctly_refused"] += overlaps
                    totals["wrongly_refused"] += not overlaps
                if target[0] not in held_files:
                    held_files.add(target[0])
                    totals["accepted_file_level"] += 1
            for client in clients:
                client.close()
        totals["missed_overlaps"] = totals["same_function_overlaps"] - totals["correctly_refused"]
        totals["parallelism_gain"] = round(totals["accepted_function_level"] / totals["accepted_file_level"], 2)
        results[str(agents)] = totals
    results["setup"] = {"files": files, "functions_per_file": functions, "trials": trials,
                        "targets": "each agent picks a random (file, function)"}
    return results


def preview(args):
    session_id, users = Fixture(args.db, args.secret).session(1, "bench-preview")
    page = ("<!doctype html><html><body>" + "<p>preview benchmark payload</p>" * 64 + "</body></html>").encode()
    token = {}
    got_token = threading.Event()

    def on_message(client, message, _received_at):
        kind = message.get("type")
        if kind == "preview" and message.get("token"):
            token["value"] = message["token"]
            got_token.set()
        elif kind == "preview_request":
            client.send({"type": "preview_response", "id": message["id"], "status": 200,
                         "headers": [["content-type", "text/plain"]],
                         "body": base64.b64encode(page).decode()})

    clients = connect_all(args, session_id, users, on_message)
    clients[0].send({"type": "preview_start", "port": 5173})
    if not got_token.wait(5):
        raise RuntimeError("no preview token")
    url = f"{args.base}/preview/{token['value']}/asset"

    def fetch(_):
        start = NOW()
        with urllib.request.urlopen(url, timeout=30) as response:
            response.read()
            ok = response.status == 200
        return (NOW() - start) * 1000, ok

    results = {"response_bytes": len(page)}
    for concurrency in (1, 10, 50):
        total = 200
        started = NOW()
        with concurrent.futures.ThreadPoolExecutor(concurrency) as pool:
            outcomes = list(pool.map(fetch, range(total)))
        elapsed = NOW() - started
        results[str(concurrency)] = {
            **percentiles([ms for ms, _ in outcomes]),
            "ok": sum(ok for _, ok in outcomes),
            "requests_per_sec": round(total / elapsed, 1),
        }
    for client in clients:
        client.close()
    return results


CONVERSATION = [
    (0, "morning all, let's plan the data pipeline today"),
    (1, "morning!"),
    (2, "hey"),
    (3, "I'll start on ingestion, reading the raw csv exports"),
    (4, "nice"),
    (1, "I'm also taking ingestion, I already have a reader for the s3 bucket"),
    (0, "ok"),
    (2, "what format should ingestion hand to training, parquet or csv?"),
    (3, "good question"),
    (4, "lol"),
    (1, "anyone know if the s3 bucket has versioning on?"),
    (0, "thanks"),
    (2, "I'll do feature engineering in features.py"),
    (3, "cool"),
    (4, "I can take evaluation and the metrics dashboard"),
    (0, "sounds good"),
    (1, "wait, I thought features.py was mine, I started on it yesterday"),
    (2, "hmm"),
    (3, "ok"),
    (0, "how are we splitting the train and test data, by date or random?"),
    (4, "yes"),
    (1, "by date makes sense for time series"),
    (2, "ok thanks"),
    (3, "brb coffee"),
    (4, "lol same"),
    (0, "should training use the GPU box or cloud spot instances?"),
    (1, "nice"),
    (2, "I'd say spot instances, cheaper"),
    (3, "ok"),
    (4, "agreed"),
    (0, "who's writing the tests for ingestion?"),
    (1, "haha"),
    (2, "not me, I'm on features"),
    (3, "back"),
    (4, "ok"),
    (0, "the old pipeline used pandas, are we keeping that or moving to polars?"),
    (1, "polars is way faster for our sizes"),
    (2, "cool"),
    (3, "I already wrote ingestion with pandas though"),
    (4, "lol"),
    (0, "ok"),
    (1, "can we keep pandas in ingestion and use polars downstream?"),
    (2, "nice"),
    (3, "works for me"),
    (4, "thanks"),
    (0, "what's the deadline for the first training run?"),
    (1, "friday I think"),
    (2, "ok"),
    (3, "yes"),
    (4, "cool"),
    (0, "ok final split: Ana ingestion with pandas, Ben features, Cai training on spot instances, Dee evaluation and tests. Agreed?"),
    (1, "agreed"),
    (2, "agreed, let's build it"),
    (3, "yes"),
    (4, "ok"),
    (0, "great"),
    (1, "thanks"),
    (2, "lol"),
    (3, "ok"),
    (4, "nice"),
]

SMALL_TALK = {"ok", "okay", "k", "thanks", "thank", "you", "thx", "ty", "lol", "haha", "nice", "cool",
              "great", "yes", "no", "yep"}


def worth_considering(text: str) -> bool:
    """Python port of listen::worth_considering, to count what's filtered for free."""
    lowered = text.strip().lower()
    if "?" in lowered:
        return True
    words = [w for w in "".join(c if (c.isalnum() or c == "'") else " " for c in lowered).split()]
    return len(words) >= 4 or (len(words) >= 2 and not all(w in SMALL_TALK for w in words))


def listen(args):
    session_id, users = Fixture(args.db, args.secret).session(5, "bench-listen", listens=True)
    metrics = {"value": None}
    agent_replies = []
    last_human = {"at": None, "text": None}
    lock = threading.Lock()

    def on_message(client, message, received_at):
        if client is not observer[0]:
            return
        kind = message.get("type")
        if kind == "metrics_update":
            metrics["value"] = message.get("metrics")
        elif kind == "chat" and message.get("sender") == "agent":
            with lock:
                agent_replies.append({
                    "after_message": last_human["text"],
                    "latency_s": round(received_at - last_human["at"], 2) if last_human["at"] else None,
                    "text": message.get("text", "")[:200],
                })

    observer = [None]
    clients = connect_all(args, session_id, users, on_message)
    observer[0] = clients[0]
    started = NOW()
    pace = args.pace
    for speaker, text in CONVERSATION:
        with lock:
            last_human["at"] = NOW()
            last_human["text"] = text
        clients[speaker].send({"type": "chat", "text": text})
        time.sleep(pace)
    # Let pending decisions (cooldown waits, a reference write) finish.
    time.sleep(45)
    elapsed = NOW() - started
    for client in clients:
        client.close()
    final = metrics["value"] or {}
    considered = sum(worth_considering(text) for _, text in CONVERSATION)
    return {
        "messages": len(CONVERSATION),
        "filtered_free": len(CONVERSATION) - considered,
        "worth_considering": considered,
        "llm_calls": final.get("llm_calls", 0),
        "estimated_cost_usd": final.get("estimated_cost_usd", 0.0),
        "agent_replies": agent_replies,
        "wall_clock_s": round(elapsed, 1),
        "pace_s": pace,
    }


def prod(args):
    host = "blueprintserver-production.up.railway.app"
    cold = []
    for _ in range(5):
        start = NOW()
        connection = http.client.HTTPSConnection(host, timeout=15)
        connection.request("GET", "/sessions")
        connection.getresponse().read()
        connection.close()
        cold.append((NOW() - start) * 1000)
    connection = http.client.HTTPSConnection(host, timeout=15)
    connection.request("GET", "/sessions")
    connection.getresponse().read()
    warm = []
    for _ in range(20):
        start = NOW()
        connection.request("GET", "/sessions")
        connection.getresponse().read()
        warm.append((NOW() - start) * 1000)
    connection.close()
    return {"new_connection_with_tls": percentiles(cold), "kept_alive_round_trip": percentiles(warm)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("section", choices=["broadcast", "collisions", "preview", "listen", "prod"])
    parser.add_argument("--base", default="http://127.0.0.1:3919")
    parser.add_argument("--db")
    parser.add_argument("--secret", default="bench")
    parser.add_argument("--pace", type=float, default=3.0, help="seconds between chat lines (listen)")
    args = parser.parse_args()
    result = {"broadcast": broadcast, "collisions": collisions, "preview": preview,
              "listen": listen, "prod": prod}[args.section](args)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    sys.exit(main())
