from datetime import date
from typing import Dict, List, Optional

from .models import Book, Loan, Member


class Store:
    """Keeps books, members and loans in memory."""

    def __init__(self) -> None:
        self.books: Dict[int, Book] = {}
        self.members: Dict[int, Member] = {}
        self.loans: Dict[int, Loan] = {}
        self._next_id = 1

    def _id(self) -> int:
        value = self._next_id
        self._next_id += 1
        return value

    def add_book(self, title: str, author: str, year: int) -> Book:
        book = Book(self._id(), title, author, year)
        self.books[book.id] = book
        return book

    def add_member(self, name: str, email: str) -> Member:
        member = Member(self._id(), name, email)
        self.members[member.id] = member
        return member

    def add_loan(self, book_id: int, member_id: int, on: date) -> Loan:
        loan = Loan(self._id(), book_id, member_id, on)
        self.loans[loan.id] = loan
        return loan

    def book(self, book_id: int) -> Optional[Book]:
        return self.books.get(book_id)

    def member(self, member_id: int) -> Optional[Member]:
        return self.members.get(member_id)

    def open_loans_for_member(self, member_id: int) -> List[Loan]:
        return [loan for loan in self.loans.values() if loan.member_id == member_id and loan.is_open]

    def open_loan_for_book(self, book_id: int) -> Optional[Loan]:
        for loan in self.loans.values():
            if loan.book_id == book_id and loan.is_open:
                return loan
        return None
