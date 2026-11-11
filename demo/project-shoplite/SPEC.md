# Shoplite: what we're building

A small online shop backend in plain Python 3.9 (standard library only). Four people build it
at once; each owns a part. Every part comes with unit tests under `tests/`, run with
`python3 -m unittest`.

## Parts and owners

| Part | Module | Owner |
|---|---|---|
| Catalog: products with id, name, price, category; search by name and category | `shoplite/catalog.py` | Alice |
| Inventory: stock per product; reserve, release and restock; never below zero | `shoplite/inventory.py` | Alice |
| Cart: add, remove, change quantity; totals; a cart can't hold more than is in stock | `shoplite/cart.py` | Bob |
| Checkout and orders: turn a cart into an order, reserve stock, apply discount codes | `shoplite/orders.py` | Bob |
| Money and discounts: a `Money` type and discount codes | `shoplite/money.py` | Carol |
| HTTP API over all of it (`http.server`, JSON) and a seeded demo store | `shoplite/api.py`, `shoplite/app.py` | Dave |

## Shared rules

- Money is never a float. Prices and totals use the `Money` type from `shoplite/money.py`.
- Ids are strings.
- Errors raise `ShopError` (from `shoplite/errors.py`) with a message a customer could read.
- Decisions made in the team chat override this file.
