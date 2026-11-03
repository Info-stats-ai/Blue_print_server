# I multi-user benchmark — results

Run 2026-11-02 on a MacBook (Apple silicon), debug build of `blueprint-server`
on localhost:3919 with a fresh SQLite database. Reproduce with
`bench/bench.py` (standard-library Python; see its docstring).

Everything below except §5 is **measured on localhost**: it is the server's own
cost. In production add the network round trip to Railway (§5, ~87 ms warm).

## 1. Broadcast latency (chat, cursors) and claim round trip

Every simulated user sends 20 chat messages and 20 cursor moves (one pair
every 50 ms) at the same time; latency is send → receipt at every *other* user
(same clock). Chat messages are also written to SQLite on the way.

| Users | Deliveries (all received) | Chat p50 / p95 / max | Cursor p50 / p95 | Claim round trip p50 |
|---|---|---|---|---|
| 5  | 800 / 800       | 2.4 / 11.0 / 21.1 ms | 2.4 / 11.0 ms | 0.7 ms |
| 10 | 3,600 / 3,600   | 3.7 / 13.5 / 20.0 ms | 3.7 / 13.5 ms | 1.4 ms |
| 25 | 24,000 / 24,000 | 1.3 / 3.8 / 15.5 ms  | 1.3 / 3.8 ms  | 1.0 ms |

Delivery rate reached ~21,500 messages/s at 25 users; that is the rate this
test *sent* at, not the server's ceiling. The first (cold) run had a 5-user p95
of 56 ms from warm-up; the table is the second run.

## 2. Collision detection: function-level vs file-level claims

N agents each claim one random function among 4 files × 10 functions, 5 trials
each. Counted: overlaps (same function already held), what the server refused,
and how many agents a file-level lock (one agent per file) would have allowed.

| Agents | Claims | Same-function overlaps | Refused | Missed / wrongly refused | Agents working (function-level) | (file-level) | Parallelism gain |
|---|---|---|---|---|---|---|---|
| 5  | 25  | 1  | 1  | 0 / 0 | 24 | 17 | **1.4×** |
| 10 | 50  | 4  | 4  | 0 / 0 | 46 | 20 | **2.3×** |
| 25 | 125 | 39 | 39 | 0 / 0 | 86 | 20 | **4.3×** |

Every true same-function overlap was caught and nothing else was refused. The
gain grows with team size because file locks cap concurrency at the number of
files. No AI is involved: each claim costs a set lookup (~1 ms round trip, §1).

## 3. Listening team agent: cost and latency

5 users replay a scripted 60-message team chat (small talk, questions, two
ownership clashes, a tooling debate, a final agreement) at one message every
3 s, with the listening agent on. The digest job was disabled so only the
listener's spend is counted.

| Messages | Filtered free (small talk) | Sent to the model's decision | Model calls | Replies (+ reference writes) | Estimated cost |
|---|---|---|---|---|---|
| 60 | 33 (55%) | 27 | 16 | 8 (+4) | **$0.0057** |

- Reply latency: 0.2–4.0 s after the last message (median ≈1.2 s); writing the
  shared reference took ≈10 s.
- ≈ $0.0001 per chat message. A busy team at 120 messages/hour ≈ **$0.01/hour**;
  this compressed run (≈960 messages/hour) ≈ $0.09/hour.
- Cost is the server's own estimate (token counts × its built-in Haiku-class
  rates in `agent::model_rates_per_million`), not an invoice.
- Observed behaviour worth tuning: it rewrote the reference 4 times (once early,
  after a partial decision); replies caught both ownership clashes and answered
  the factual question.

## 4. Shared preview relay

One sharer answers relayed requests with a 2 KB page; viewers fetch
`/preview/<token>/asset` through the server, 200 requests per level.

| Concurrent viewers' requests | OK | p50 / p95 / max | Requests/s |
|---|---|---|---|
| 1  | 200 / 200 | 2.8 / 14.7 / 79.2 ms   | 178 |
| 10 | 200 / 200 | 12.3 / 30.3 / 54.1 ms  | 581 |
| 50 | 200 / 200 | 76.3 / 111.8 / 134.0 ms | 545 |

Each request crosses the server twice (viewer → server → sharer's IDE → back).
Here the "sharer" is a script answering instantly, so a real dev server's own
time comes on top.

## 5. Production network round trip (Railway)

`GET /sessions` to `blueprintserver-production.up.railway.app` from the same
laptop, 20 samples.

| | p50 | p95 |
|---|---|---|
| Kept-alive connection (what an open websocket feels like) | 87 ms | 97 ms |
| New connection incl. TLS handshake | 221 ms | 322 ms |

So a teammate's keystroke or cursor reaches others in roughly one round trip
(~90 ms here) plus the few ms measured in §1.

## What this does and doesn't show

- **Shows:** the server adds single-digit milliseconds at 25 users; function-level
  claims catch every same-function overlap while letting up to 4.3× more agents
  work; the listening agent costs about a cent per busy hour and filters half
  of chat for free.
- **Doesn't show:** a comparison with Zed's own collaboration (not measured);
  behaviour across real networks from several locations (one laptop, one
  region); a release build; AI quality beyond this one scripted chat.
- **Test-harness limit:** all simulated users share one IP, so the server's
  per-IP rate limit (100 then 1 per 600 ms) throttled websocket connects
  between sections; the benchmark waits it out. Real teammates come from
  different IPs.
