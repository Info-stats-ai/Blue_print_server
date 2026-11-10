from datetime import date

from .models import Loan
from .store import Store

MAX_OPEN_LOANS = 3


class LendingError(Exception):
    """A borrow or return that the rules don't allow."""


class LendingService:
    def __init__(self, store: Store) -> None:
        self.store = store

    def borrow(self, book_id: int, member_id: int, on: date) -> Loan:
        if self.store.book(book_id) is None:
            raise LendingError(f"No book {book_id}")
        if self.store.member(member_id) is None:
            raise LendingError(f"No member {member_id}")
        if self.store.open_loan_for_book(book_id) is not None:
            raise LendingError("That book is already on loan")
        if len(self.store.open_loans_for_member(member_id)) >= MAX_OPEN_LOANS:
            raise LendingError(f"Members can have at most {MAX_OPEN_LOANS} books out")
        return self.store.add_loan(book_id, member_id, on)

    def give_back(self, book_id: int, on: date) -> Loan:
        loan = self.store.open_loan_for_book(book_id)
        if loan is None:
            raise LendingError("That book isn't on loan")
        loan.returned_on = on
        return loan
