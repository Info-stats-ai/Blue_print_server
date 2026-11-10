import unittest
from datetime import date

from bookshelf.service import MAX_OPEN_LOANS, LendingError, LendingService
from bookshelf.store import Store


class LendingTests(unittest.TestCase):
    def setUp(self):
        self.store = Store()
        self.member = self.store.add_member("Ada", "ada@example.test")
        self.books = [self.store.add_book(f"Book {n}", "Author", 2000 + n) for n in range(5)]
        self.service = LendingService(self.store)

    def test_borrow_and_return(self):
        loan = self.service.borrow(self.books[0].id, self.member.id, date(2026, 1, 1))
        self.assertTrue(loan.is_open)
        self.service.give_back(self.books[0].id, date(2026, 1, 5))
        self.assertFalse(loan.is_open)

    def test_a_book_is_on_one_loan_at_a_time(self):
        self.service.borrow(self.books[0].id, self.member.id, date(2026, 1, 1))
        other = self.store.add_member("Linus", "linus@example.test")
        with self.assertRaises(LendingError):
            self.service.borrow(self.books[0].id, other.id, date(2026, 1, 2))

    def test_members_have_a_limit(self):
        for book in self.books[:MAX_OPEN_LOANS]:
            self.service.borrow(book.id, self.member.id, date(2026, 1, 1))
        with self.assertRaises(LendingError):
            self.service.borrow(self.books[MAX_OPEN_LOANS].id, self.member.id, date(2026, 1, 1))


if __name__ == "__main__":
    unittest.main()
