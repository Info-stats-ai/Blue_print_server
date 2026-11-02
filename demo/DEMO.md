# I-mode demo — recording script

About 8–10 minutes (skip optional scenes for 6). Two people, Alice (leader)
and Bob, fix a bug together in **Tiny Tasks** while their AI agents help.

## Before you record

```bash
~/blueprint-server/demo/start-demo.sh
```

- Two IDE windows open: Alice's and Bob's. Put them side by side, Alice on the left.
- In each window, open the I-mode panel (the **I-mode** button, top right) if
  it isn't open already.
- Wait until both feeds say they're connected and the header shows 2 people.
- Everything runs locally (test accounts, local server). Your own IDE window
  isn't touched.

When you're done: `~/blueprint-server/demo/stop-demo.sh`

## Scene 1 — Two people, one session (30s)

1. Point at the session header in Alice's panel: "Tiny Tasks", Alice is the leader.
2. Point at the people count and the Collaborators list in both windows.
3. Say: "Two developers, two machines, one shared session."

## Scene 2 — Team chat (30s)

1. In Bob's window, make sure **Send to: Team** is selected, type:
   `Hey Alice, completing task 1 marks the wrong task — can you look?` and send.
2. The message shows up instantly in Alice's window.

## Scene 3 — See who's working where (45s)

1. In Alice's window, open `tasks/store.py`.
2. In Bob's window, the panel shows Alice is in `tasks/store.py`.
3. Bob opens the same file — both now see each other there.

## Scene 4 — Live co-editing (45s)

1. Alice types a comment above `complete`, e.g. `# looking at this now`.
2. It appears in Bob's editor as she types.
3. Bob deletes it — it disappears from Alice's editor.

## Scene 5 — Ask your own AI (1 min)

1. In Alice's window, show the chat box: model picker (Claude, with its ▾),
   effort picker, Ask / Plan / Agent.
2. Pick **Ask**, switch **Send to: My AI**, and type:
   `Why does completing task 1 mark the second task?`
3. While it answers, the send button is a red **Stop** — point it out.
4. The answer explains `complete()` uses the id as a list index (off by one).

## Scene 6 — Let the agent fix it (1.5 min)

1. Switch to **Agent**, type:
   `Fix complete() in tasks/store.py so it finds the task by id, add a test for it, then run the tests.`
   (Or, on **Team**, end any request with `@agent` — it goes to your own AI,
   which can change files, and **Send to** switches to **My AI** for you.)
2. The agent edits the file and adds a test — the steps show inline.
3. When it wants to run the tests, a **Run / Deny** card appears. Click **Run**.
4. Tests pass. Click **Sync to team** on the banner: Bob's copy gets the fix.

## Scene 7 — An AI teammate that listens (1.5 min)

1. In Alice's window, open the people menu (next to the I-mode switch) and turn
   on **Team agent follows the chat**. Both feeds say it's on.
2. On **Team**, Bob types `I'll take the tests and the CLI` and Alice types
   `I'm doing the CLI too`. The agent points out the overlap by itself.
3. Bob types `lol` — the agent stays quiet.
4. Alice types `ok: Bob does tests, I do the CLI. Agreed?`, Bob answers
   `agreed, let's build it`. The agent confirms the split and writes
   `REFERENCE.md`, which appears in both file trees with a Work Split by name.

## Scene 8 — Two agents, one file (optional, 1 min)

1. Both open `tasks/store.py`. Bob asks his AI to change `complete()`; while it
   works, Alice asks hers to change `complete()` too.
2. Alice's agent backs off — "Collision avoided: … is already changing
   `TaskStore.complete`" — while edits to other functions in the same file go
   through.

## Scene 9 — Show the running app (45s)

1. In a terminal in Alice's project: `python3 -m http.server 5173`.
2. People menu → **Share running app…** → `5173` → **Share**.
3. Bob's session header shows "alice's app · port 5173" with **Open**: it opens
   Alice's localhost in Bob's browser, through the server.
4. People menu → **Stop sharing app** — the link stops working.

## Scene 10 — The numbers (30s)

1. Open the **Team** tab: **This week** shows conflicts caught (agent
   collisions, divergences, out-of-lane edits) and live merges, per day.
2. Click **Copy summary** and paste it: one line for a status update.

## Scene 11 — Wrap-up (20s)

1. Back to both windows side by side: same files, same chat, both AIs aware of
   who did what.
2. In Bob's window, close it — Alice's feed updates to show Bob left.

## If something goes wrong

- **No Claude models in the picker:** your Anthropic key isn't saved in your
  own IDE. Add it once in your normal IDE (Agent settings), then restart the demo.
- **`@agent` replies with an error:** the demo server uses the Anthropic key
  saved in your IDE. If you changed it, restart the demo.
- **A window didn't join the session:** run `start-demo.sh` again (it starts fresh).
- Logs: `~/blueprint-server/demo/.state/`.
