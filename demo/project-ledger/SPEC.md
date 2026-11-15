# Ledger: what we're building

Invoices for a small agency: line items, tax, payments and a monthly report.
Plain Python 3.9+, standard library only, tests under `tests/` with `unittest`.

| Part | Module | Owner |
|---|---|---|
| Money: amounts in cents, adding, multiplying, formatting | `ledger/money.py` | Alice |
| Invoices: line items, subtotal, tax, total | `ledger/invoice.py` | Alice |
| Payments: record payments, balance due, paid in full | `ledger/payments.py` | Bob |
| Report: a month's invoices summed up | `ledger/report.py` | Bob |

## Shared rules

- Money is never a float: amounts are whole cents in the `Money` type.
- Tax rates are given in basis points (825 = 8.25%) and rounded half up.
