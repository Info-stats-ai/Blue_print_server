"""A small JSON API: GET /books, GET /books/<id>, POST /loans, POST /returns."""

import json
from datetime import date
from http.server import BaseHTTPRequestHandler, HTTPServer

from .service import LendingError, LendingService
from .store import Store


def make_handler(store: Store):
    service = LendingService(store)

    class Handler(BaseHTTPRequestHandler):
        def _send(self, status: int, body) -> None:
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def _body(self) -> dict:
            length = int(self.headers.get("content-length") or 0)
            return json.loads(self.rfile.read(length) or b"{}")

        def do_GET(self) -> None:
            if self.path == "/books":
                self._send(200, [vars(book) for book in store.books.values()])
            elif self.path.startswith("/books/"):
                book = store.book(int(self.path.rsplit("/", 1)[1]))
                self._send(200, vars(book)) if book else self._send(404, {"error": "not found"})
            else:
                self._send(404, {"error": "not found"})

        def do_POST(self) -> None:
            body = self._body()
            try:
                if self.path == "/loans":
                    loan = service.borrow(body["book_id"], body["member_id"], date.today())
                    self._send(201, {"loan_id": loan.id})
                elif self.path == "/returns":
                    service.give_back(body["book_id"], date.today())
                    self._send(200, {"ok": True})
                else:
                    self._send(404, {"error": "not found"})
            except LendingError as error:
                self._send(409, {"error": str(error)})

        def log_message(self, *args) -> None:
            pass

    return Handler


def serve(store: Store, port: int = 8080) -> None:
    HTTPServer(("127.0.0.1", port), make_handler(store)).serve_forever()
