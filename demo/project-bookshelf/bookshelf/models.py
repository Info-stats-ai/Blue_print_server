from dataclasses import dataclass, field
from datetime import date
from typing import Optional


@dataclass
class Book:
    id: int
    title: str
    author: str
    year: int


@dataclass
class Member:
    id: int
    name: str
    email: str


@dataclass
class Loan:
    id: int
    book_id: int
    member_id: int
    borrowed_on: date
    returned_on: Optional[date] = None

    @property
    def is_open(self) -> bool:
        return self.returned_on is None
