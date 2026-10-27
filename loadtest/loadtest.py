#!/usr/bin/env python3
"""Concurrent-user load test for blueprint-server.

Registers N distinct real user accounts against a running server, has one
create a session and the rest join it, then opens N real WebSocket
connections into that one session and drives synthetic activity (file
opens, CRDT-shaped buffer ops, chat, cursor moves) from every connection
concurrently for a fixed duration — the actual "N people editing the same
session" scenario this app is built for, not a synthetic HTTP benchmark.

Usage:
    ./venv/bin/python3 loadtest.py --users 50 --duration 30 --base-url http://localhost:3000
"""

import argparse
import asyncio
import json
import time
import uuid
from dataclasses import dataclass, field

import aiohttp
import websockets


@dataclass
class ClientStats:
    user_id: str = ""
    connected: bool = False
    connect_error: str | None = None
    messages_sent: int = 0
    messages_received: int = 0
    send_errors: int = 0
    # Round-trip latency for this client's own chat messages: send time to
    # seeing that exact message echoed back. The real end-to-end signal —
    # how long it takes a change to actually reach you — not raw throughput.
    chat_round_trip_s: list[float] = field(default_factory=list)


async def register_and_login(session: aiohttp.ClientSession, base_url: str, email: str, password: str) -> tuple[str, str]:
    async with session.post(f"{base_url}/auth/register", json={"email": email, "password": password}) as resp:
        if resp.status not in (200, 201):
            # Already registered from a previous run against the same DB — fall through to login.
            pass
    async with session.post(f"{base_url}/auth/login", json={"email": email, "password": password}) as resp:
        resp.raise_for_status()
        data = await resp.json()
        return data["token"], data["user_id"]


async def create_session(session: aiohttp.ClientSession, base_url: str, token: str, name: str) -> str:
    headers = {"Authorization": f"Bearer {token}"}
    async with session.post(f"{base_url}/sessions", json={"name": name, "has_leader": False}, headers=headers) as resp:
        resp.raise_for_status()
        data = await resp.json()
        return data["session"]["id"]


async def join_session(session: aiohttp.ClientSession, base_url: str, token: str, session_id: str) -> None:
    headers = {"Authorization": f"Bearer {token}"}
    async with session.post(f"{base_url}/sessions/{session_id}/join", headers=headers) as resp:
        resp.raise_for_status()


async def client_worker(
    index: int,
    ws_base_url: str,
    token: str,
    user_id: str,
    session_id: str,
    duration_s: float,
    stats: ClientStats,
) -> None:
    stats.user_id = user_id
    agent_id = f"loadtest-{index}-{uuid.uuid4().hex[:8]}"
    url = f"{ws_base_url}/ws?session_id={session_id}&token={token}"

    try:
        async with websockets.connect(url, open_timeout=15, close_timeout=5) as ws:
            stats.connected = True

            async def receiver():
                async for raw in ws:
                    stats.messages_received += 1
                    try:
                        msg = json.loads(raw)
                    except json.JSONDecodeError:
                        continue
                    if msg.get("type") == "chat" and msg.get("sender") == user_id:
                        sent_at = pending_chat_sends.pop(msg.get("text"), None)
                        if sent_at is not None:
                            stats.chat_round_trip_s.append(time.monotonic() - sent_at)

            pending_chat_sends: dict[str, float] = {}
            recv_task = asyncio.create_task(receiver())

            end_at = time.monotonic() + duration_s
            path = f"loadtest/file_{index % 10}.py"
            row = 0
            tick = 0
            try:
                while time.monotonic() < end_at:
                    tick += 1
                    try:
                        if tick % 5 == 0:
                            text = f"ping from {agent_id} #{tick}"
                            pending_chat_sends[text] = time.monotonic()
                            await ws.send(json.dumps({"type": "chat", "text": text}))
                        elif tick % 5 == 1:
                            await ws.send(json.dumps({"type": "file_open", "sender": agent_id, "path": path}))
                        elif tick % 5 == 2:
                            row = (row + 1) % 200
                            await ws.send(json.dumps({"type": "cursor_moved", "sender": agent_id, "path": path, "row": row}))
                        elif tick % 5 == 3:
                            # A minimal, well-formed insert-only CRDT op shape —
                            # exercises the same apply/broadcast path real edits
                            # do without needing a real client-side text::Buffer
                            # to construct a "valid" operation from.
                            await ws.send(json.dumps({
                                "type": "buffer_op",
                                "path": path,
                                "op": {
                                    "timestamp": {"replica_id": index, "value": tick},
                                    "version": [],
                                    "edits": [[0, 0, f"x{tick}"]],
                                },
                            }))
                        else:
                            await ws.send(json.dumps({"type": "file_save", "path": path, "content": f"# loadtest {tick}\n"}))
                        stats.messages_sent += 1
                    except Exception:
                        stats.send_errors += 1
                    await asyncio.sleep(0.2)
            finally:
                recv_task.cancel()
                try:
                    await recv_task
                except (asyncio.CancelledError, Exception):
                    pass
    except Exception as error:
        stats.connect_error = f"{type(error).__name__}: {error}"


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--users", type=int, default=50)
    parser.add_argument("--duration", type=float, default=30.0, help="seconds of activity per client after connecting")
    parser.add_argument("--base-url", default="http://localhost:3000")
    parser.add_argument("--ws-base-url", default=None, help="defaults to base-url with ws:// scheme")
    args = parser.parse_args()

    ws_base_url = args.ws_base_url or args.base_url.replace("http://", "ws://").replace("https://", "wss://")
    run_id = uuid.uuid4().hex[:8]

    async with aiohttp.ClientSession() as session:
        print(f"Registering/logging in {args.users} users (staggered — see README on the per-IP rate limiter)…")
        creds = [(f"loadtest-{run_id}-{i}@example.com", "loadtest-password-123") for i in range(args.users)]
        tokens_and_ids = []
        # All simulated users share one real IP (this machine), unlike real
        # distinct users. The /auth/* routes specifically are limited to a
        # burst of 10 requests per IP, refilling 1 every 6s (main.rs's
        # auth_governor_conf) — stricter than the global limiter, and
        # discovered by this script hitting a 429 before this pacing was
        # added. Each user needs 2 auth calls (register + login), so this
        # spends the initial burst on the first 5 users, then paces the
        # rest at the server's actual refill rate rather than guessing.
        auth_burst_users = 5
        for i, (email, pw) in enumerate(creds):
            if i >= auth_burst_users:
                # Each user costs 2 tokens (register + login), and the
                # bucket refills only 1 token per 6s — so a fresh user past
                # the initial burst needs ~12.4s, not 6.2s, to not 429 on
                # the second (login) call. Confirmed by this script 429ing
                # on /auth/login specifically until this was accounted for.
                await asyncio.sleep(12.4)
            tokens_and_ids.append(await register_and_login(session, args.base_url, email, pw))

        print("Creating shared session…")
        owner_token, _owner_id = tokens_and_ids[0]
        session_id = await create_session(session, args.base_url, owner_token, f"loadtest-{run_id}")
        print(f"Session: {session_id}")

        print(f"Joining all {args.users} users to it…")
        await asyncio.gather(
            *[join_session(session, args.base_url, token, session_id) for token, _ in tokens_and_ids]
        )

    print(f"Opening {args.users} concurrent WebSocket connections, running {args.duration}s of activity each…")
    stats_list = [ClientStats() for _ in range(args.users)]
    start = time.monotonic()
    await asyncio.gather(
        *[
            client_worker(i, ws_base_url, token, user_id, session_id, args.duration, stats_list[i])
            for i, (token, user_id) in enumerate(tokens_and_ids)
        ]
    )
    elapsed = time.monotonic() - start

    connected = sum(1 for s in stats_list if s.connected)
    failed = [s for s in stats_list if not s.connected]
    total_sent = sum(s.messages_sent for s in stats_list)
    total_received = sum(s.messages_received for s in stats_list)
    total_send_errors = sum(s.send_errors for s in stats_list)
    all_round_trips = [t for s in stats_list for t in s.chat_round_trip_s]

    print("\n=== Load test results ===")
    print(f"Users requested:      {args.users}")
    print(f"Connected OK:         {connected}/{args.users}")
    if failed:
        print(f"Connection failures:  {len(failed)}")
        for s in failed[:5]:
            print(f"  - {s.user_id}: {s.connect_error}")
        if len(failed) > 5:
            print(f"  ...and {len(failed) - 5} more")
    print(f"Wall time:            {elapsed:.1f}s")
    print(f"Messages sent:        {total_sent}")
    print(f"Messages received:    {total_received}")
    print(f"Send errors:          {total_send_errors}")
    if all_round_trips:
        sorted_rt = sorted(all_round_trips)
        p50 = sorted_rt[len(sorted_rt) // 2]
        p95 = sorted_rt[int(len(sorted_rt) * 0.95)]
        p99 = sorted_rt[int(len(sorted_rt) * 0.99)]
        print(f"Chat round-trip p50:  {p50 * 1000:.0f}ms")
        print(f"Chat round-trip p95:  {p95 * 1000:.0f}ms")
        print(f"Chat round-trip p99:  {p99 * 1000:.0f}ms")
    else:
        print("Chat round-trip:      no samples (all connections failed before sending)")


if __name__ == "__main__":
    asyncio.run(main())
