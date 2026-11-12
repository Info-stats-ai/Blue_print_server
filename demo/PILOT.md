# Pilot with real testers

Goal: find out whether 2–4 people, each with their own AI, can build one small project together in
I-IDE without stepping on each other, and where they get stuck.

## Before the first session (the host)

1. Package the app: `ZED_RELEASE_CHANNEL=stable CARGO_INCREMENTAL=0 cargo build --release -p i-ide && script/package-tester-mac`
   (in `~/i-ide`). The DMG lands in `target/tester/`. Apple silicon only.
2. Send each tester the DMG (Google Drive or AirDrop) and the link https://i-ide.dev/docs/installation.
3. Check everyone can sign in with GitHub or Google. If Google says "access blocked", the Google
   OAuth app is still in Testing mode: add the tester's address under *OAuth consent screen > Test users*.
4. Each tester sets up their own AI in the Agent panel (their own Anthropic key or Claude account).
   My AI on the right uses the session's AI, which has a daily cap per session.
5. The host unzips `pilot-shoplite.zip` (this folder's `project-shoplite`) and opens it in I-IDE.

## Invite message (copy and edit)

> Hi! I'm testing I-IDE, a code editor where each person's AI can see what the others are doing.
> Could you spare 45 minutes on <day> with <names>? You'll need an Apple silicon Mac.
> Install: https://i-ide.dev/docs/installation (I'll send you the file). Sign in with GitHub or
> Google. During the session we'll build a tiny shop backend together, and afterwards I'll ask five
> quick questions. Nothing you type is shared outside the session.

## Session script (45 minutes)

| Time | What happens | What we watch for |
|---|---|---|
| 0–5 | Host starts a session, sends the invite link, approves each join request | Did anyone fail to join? How long did it take? |
| 5–8 | Everyone answers "How should files move in this team?" | Which choices people make; whether the card was clear |
| 8–13 | Host switches the message box to **Plan**, asks for a plan for SPEC.md, answers its questions, then presses **Share with team** | Were the questions useful? Did each person get a task in **Team activity**? |
| 13–25 | Each person asks their Agent panel to build their task with tests | Do agents use teammates' real code instead of stand-ins? Any collisions? Does Team activity show who is editing what? |
| 25–28 | Host pins one decision in chat (e.g. "Order ids look like ORD-0001") | Do running agents follow it? |
| 28–38 | Each person reviews one teammate's agent run: accept, or send back with a note | Was review easy to find? Was the note acted on? |
| 38–45 | Someone runs `python3 -m unittest`; the team fixes what fails | Do all parts fit together? |

The host stays quiet unless someone is stuck for more than two minutes. Write down every place
someone asked "where is…?" or "what does this mean?".

## Afterwards: five questions

1. What were you trying to do when you felt most lost?
2. Did your AI know what your teammates had built? Give one example.
3. Which message or button in I-mode or Team activity made no sense?
4. Would you use this with your real team? Why or why not?
5. One thing you'd change first.

For problems, testers choose **Help > Report a Problem**: it copies their system details, which they
paste into a message to the host with what they did, expected and saw (a screenshot helps).

## What we measure

- Time from opening the invite link to being in the session.
- Files shared automatically vs. by pressing Sync; files held for review.
- Agent runs accepted vs. sent back.
- Plan mode: questions asked, tasks created from the shared plan, tasks finished.
- Collisions: two agents editing the same function.
- Tests passing at the end.
- The `/metrics/weekly` numbers for the session.

## Phases

1. **Guided pilot:** 2 teams of 2–3 trusted friends, one session each, host watching.
2. **Fix:** change what the pilot showed, then send a new DMG.
3. **On their own:** 5–10 teams use it for a week on their own project, with Report a Problem and a weekly check-in.
