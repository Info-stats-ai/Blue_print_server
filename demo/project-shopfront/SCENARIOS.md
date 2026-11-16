# Shopfront: three people at once

Alice, Bob and Carol, each with their own AI, in one session. Start it with:

    DEMO_PEOPLE="alice bob carol" DEMO_PROJECT=project-shopfront ./demo/start-demo.sh

Alice owns buying (cart, orders, store), Bob owns stock and delivery
(inventory, shipping, returns), Carol owns money (pricing, coupons, payments,
reports). Every step is done in the real app, by typing and clicking. A
scenario passes only when every expected result is seen on every screen it
names. Run the whole sheet once with everyone on **Auto** sync and once on
**Manual**.

## A. Three people change three files at the same moment

1. Alice adds `Cart.clear()` in `shop/cart.py`; Bob adds
   `Inventory.total_on_hand()` in `shop/inventory.py`; Carol adds
   `PaymentGateway.charges_for(order_id)` in `shop/payments.py`. All three
   save within a few seconds of each other.

Expected: all three files reach all three people; nobody sees a conflict;
`python3 -m unittest` passes in all three copies; the three copies are
identical.

Measure: save to arrival on the other two screens.

## B. One person's change reaches into the other two people's code

1. Carol adds a required `express: bool` to `quote(...)` in
   `shop/pricing.py` and saves.

Expected:
- Carol: **Your change breaks callers**, naming `shop/store.py` (Alice's) and
  the tests, before anyone else is hurt.
- Alice: a warning card naming her call in `shop/store.py`, and her tests run
  on their own and show what failed.
- Bob: no warning (his code doesn't call `quote`), but he sees the change.

2. Carol clicks **Fix the calls too**.

Expected: her AI updates the callers; Alice's warning clears on its own; all
three people's tests pass.

## C. An AI's wrong claim doesn't spread (the safeguards)

1. Bob asks his AI to add `Inventory.reorder_list()`; while it works, Bob's AI
   states something about the code in its steps.
2. Alice opens Team activity and clicks **Flag as wrong** on Bob's run, typing
   why.

Expected:
- All three see "Flagged as wrong by Alice" on that run, with her reason.
- Carol asks her AI "what did Bob's agent just do?": her AI doesn't present
  the flagged run as fact, and says it was flagged.
- Alice clicks **Undo**: the flag disappears for all three.

3. Carol clicks **Accept** on a later run of Bob's; Alice clicks **Needs
   changes** on another.

Expected: team memory serves the accepted one and not the one sent back
(ask an AI about each).

## D. A decision replaces an older one

1. Alice pins "Returns are accepted within 30 days".
2. Bob pins "Returns are accepted within 14 days from now on".

Expected: Bob is asked **Replace it / Keep both / Cancel** and sees Alice's
decision. He clicks **Replace it**: all three now see only the 14-day
decision, and an AI asked "how long do customers have to return?" says 14.

## E. Intent before edit, three people

1. Bob (Agent mode): "Add a required `reason: str` to `Inventory.release` and
   update every caller, then run the tests."

Expected, while Bob's AI works:
- Alice sees **Changes on the way** naming her call in `shop/orders.py`.
- Carol sees it too only if her code calls it (it doesn't): no card.
- Neither Alice nor Carol receives a half-finished file or a failing test.
- When the task ends, every file it changed arrives together and all three
  test suites pass.

## F. Branches with three people

1. Carol: `git checkout -b feature/express-shipping` in her terminal.

Expected: People shows Carol on another branch. Alice and Bob keep sharing
files on main; Carol gets their changes as context, never as files, and they
get hers the same way. If Carol and Alice both change `Store.checkout`, all
three see the merge forecast.

## G. Someone joins late, someone drops

1. Carol quits the app mid-session; Alice and Bob keep working (two saves, one
   pinned decision, one flag).
2. Carol reopens.

Expected: she gets the two files, the decision and the flag without asking;
nothing she had is overwritten without review.

## H. Solo

1. Quit all. Open the app with no project.

Expected: the home screen (not a blank file), with **Solo** and **I-mode**,
and no team panel beside it.

2. On the **Solo** card, click **Open a project** and choose this folder.

Expected: no question (Solo was already chosen). It behaves like a normal
editor with one AI chat; nothing is shared; the model is Claude. The switch at
the top of the chat turns I-mode on at any time.

3. Quit, reopen, and click this project under **Pick up where you left off**.

Expected: asked **Solo or I-mode**. **I-mode** opens the team panel ready to
start a session; **Solo** leaves it a normal editor; closing the question
changes nothing.

## I. Build one big feature end to end, in modules

Gift cards: sold as a product, kept as a balance, spent at checkout, shown in
reports. The work is split so each person's AI owns one module and nobody
edits another person's files.

1. Alice (Plan mode): "Plan gift cards for Shopfront. A new module
   `shop/giftcards.py` holds cards and balances. Checkout can spend a card
   before charging the bank card. Reports show gift card sales. Split it
   between Alice (checkout in `shop/store.py` and `shop/orders.py`), Bob
   (delivery: a gift card has no weight and needs no stock) and Carol (the
   `giftcards` module, payments and reports). Tests for every part."
2. Alice shares the plan with the team.

Expected: the plan names a module and an owner for every step; after
**Share with team**, all three see the same tasks under Team activity, each
with its owner.

3. Each person asks their own AI (Agent mode) to do their tasks, all three at
   the same time.

Expected:
- Each AI stays in its own files; anyone whose code is about to be touched
  sees **Changes on the way** first.
- Nobody receives a half-finished file or a failing test from someone else.
- If one AI changes something another person's code calls, that person is
  told and their tests run on their own.
- At the end the three copies are identical and `python3 -m unittest` passes
  in each, including the new gift card tests.

Measure: time from the first request to all tests passing on all three
screens, and the tokens each AI used.
