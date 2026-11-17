# Shopfront

The back end of a small online store: a catalog, carts, pricing with coupons
and loyalty discounts, stock, orders, card payments, shipping, returns,
customer messages and sales reports.

    python3 -m unittest

Everything is plain Python with no packages to install. Money is whole cents.

| Area | Files | Who usually owns it |
|---|---|---|
| Buying | `shop/cart.py`, `shop/orders.py`, `shop/store.py` | Alice |
| Stock and delivery | `shop/inventory.py`, `shop/shipping.py`, `shop/returns.py` | Bob |
| Money | `shop/pricing.py`, `shop/coupons.py`, `shop/payments.py`, `shop/reports.py` | Carol |
| Shared by everyone | `shop/money.py`, `shop/catalog.py`, `shop/customers.py`, `shop/notifications.py` | whoever needs it |

The storefront page is in `web/`. Serve it with `python3 -m http.server 8765 --directory web` and open http://localhost:8765/.
