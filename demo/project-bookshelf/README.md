# Bookshelf

A small library lending service: members borrow and return books.

- `bookshelf/models.py`: `Book`, `Member`, `Loan`
- `bookshelf/store.py`: in-memory storage for books, members and loans
- `bookshelf/service.py`: lending rules (limits, availability, returns)
- `bookshelf/api.py`: a JSON HTTP API on the standard library's `http.server`
- `bookshelf/cli.py`: command-line entry point
- `tests/`: unit tests, run with `python3 -m unittest`

## Rules today

- A member can have at most 3 books out at a time.
- A book can be on one loan at a time.
