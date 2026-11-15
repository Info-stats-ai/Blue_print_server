# Ledger: I-IDE test scenarios

Two people, Alice and Bob, each with their own AI, in one session. Start it with:

    DEMO_PEOPLE="alice bob" DEMO_PROJECT=project-ledger ./demo/start-demo.sh

Every scenario is done in the real app, by typing and clicking, never by
writing files behind the app's back. Each lists what must happen and what to
measure. A scenario passes only if every expected result is seen.

Set both windows to **Auto** sync first.

## 1. Same branch: changes merge themselves

1. Alice adds `def line_count(self)` to `Invoice` in `ledger/invoice.py` and saves.
2. At the same time Bob adds `def is_empty(self)` to `Invoice` and saves.

Expected:
- Both functions end up in both copies; no conflict.
- Team activity shows `invoice.py` in sync for both.

Measure: save to arrival on the other screen (target under 1 s on one machine).

## 2. Impact preview: your change breaks callers

1. Alice changes `def total(self, rate_bp: int)` to `def total(self, rate_bp: int, discount_bp: int)` and saves.

Expected on Alice's screen, within about 2 s:
- **Your change breaks callers** card: `total(self, rate_bp: int) → total(self, rate_bp: int, discount_bp: int)`.
- Still called at `ledger/payments.py` and `ledger/report.py`, with Bob named.
- A **Fix the calls too** button.

Measure: AI tokens used (must be 0: this is local).

## 3. Radar and live affected tests on Bob's screen

Expected on Bob's screen after scenario 2 lands:
- **Breaking changes** card listing his calls in `payments.py` and `report.py`.
- **Tests after teammates' changes** runs `tests/test_invoice.py` and `tests/test_payments.py` on its own and shows **Failed** with `missing 1 required positional argument: 'discount_bp'`.
- **Ask AI to fix** on both cards.

Measure: arrival to test result (target under 5 s).

## 4. Fix the calls before anyone hits the break

1. Alice clicks **Fix the calls too**.

Expected:
- Alice's AI (a Claude model, not a local one) updates the calls in `payments.py` and `report.py`, only the calls.
- Bob receives the fixed files; his Breaking changes card clears on its own.
- His tests run again and pass.

Measure: tokens for the run (shown in Agent runs), and wall time.

## 5. Intent before edit, with Hold on

1. Bob asks his AI: "Change `Money.times` to take a `Decimal`-free `factor_bp` in basis points instead of a quantity."

Expected:
- Before Bob's file syncs, Alice sees **Changes on the way**: `times(self, quantity: int) → times(self, factor_bp: int)`, with where her code calls it.
- Alice clicks **Hold on**: Bob gets "Alice asked your AI to hold its change to times()"; `money.py` stays unsent until Bob syncs it himself.
- Bob's AI is told about the hold in its next step.

## 6. Team code memory

1. Bob asks his AI: "Add a function that returns how much a customer still owes across several accounts."

Expected:
- The request already lists `Account.balance_due` from the code index, or the AI calls `find_code`.
- The new function reuses `balance_due()` instead of re-adding payments itself.

## 7. Editor use

1. Alice asks her AI: "Rename `paid_in_full` to `is_settled` everywhere."

Expected:
- The AI uses `rename_symbol` (shown as a tool step), not a text search and edits.
- Every use is renamed, files saved and synced to Bob, tests still pass.
- Asking "Are there errors in payments.py?" uses `get_diagnostics`.

## 8. Branches: context, not files

1. In Bob's terminal: `git checkout -b feature/discounts`.

Expected:
- People shows each person's branch; a plain note says Bob is on another branch: context, not files.
- Alice edits `invoice.py` on main and saves: Bob's `invoice.py` is **not** changed. He gets a cross-branch card if the change breaks his code.
- Bob edits `report.py` on his branch: Alice's file is not changed; she sees it as context.
- Both change `Invoice.total` differently: a **merge forecast** warns that `invoice.py` will conflict when the branches merge.
- Switching Bob back to main never overwrites his files.
- The setting **Across branches: share nothing** stops all of it.

## 9. Every agent gets the tools

1. Alice opens the Agent panel's own agent and asks it to find the code that formats money.

Expected: it calls `find_code` and finds `format_money`.

2. AI & sharing → **Copy the Claude Code command**, run it in a terminal, start `claude` in the project.

Expected: `claude mcp list` shows `i-ide` connected; asking Claude Code for the references of `balance_due` uses `find_references`.

## 10. The app itself

- Quitting and reopening I with no project shows the home screen, not a blank file.
- Single-clicking two files opens two tabs.
- Every tab shows an ✕; the terminal, agent and outline panels have an ✕ in their header.
- The extension store lists 180 extensions; installing a theme works without a restart.

## 11. Failure modes

- Turn Wi-Fi off, edit, turn it back on: edits arrive, nothing is lost.
- Two saves of the same line at once: a conflict is shown to both, nothing is merged silently.
- Close a window mid-agent-run: the run is marked stopped, no half-written file is synced.
