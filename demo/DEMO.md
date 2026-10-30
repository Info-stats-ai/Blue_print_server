# I-mode demo — recording script

About 5–7 minutes. Two people, Alice (leader) and Bob, fix a bug together in
**Tiny Tasks** while their AI agents help.

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
2. The agent edits the file and adds a test — the steps show inline.
3. When it wants to run the tests, a **Run / Deny** card appears. Click **Run**.
4. Tests pass. Bob's window shows the file changed (or click **Sync to team**
   if the banner asks).

## Scene 7 — Shared team AI (1 min)

1. Switch **Send to: Team**. In Bob's window type:
   `@agent blueprint`
2. The server's agent writes a shared reference of the project and the
   conversation, and both windows get it.
3. Optional: in Alice's window, `@agent triage` (leader only) checks the files
   against that reference.

## Scene 8 — Wrap-up (20s)

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
