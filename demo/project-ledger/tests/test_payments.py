import unittest

from ledger.invoice import Invoice
from ledger.money import Money
from ledger.payments import Account
from ledger.report import summary_line


class PaymentTests(unittest.TestCase):
    def setUp(self):
        invoice = Invoice("INV-002", "Globex")
        invoice.add_line("Consulting", Money(50000), quantity=2)
        self.account = Account(invoice, 1000)

    def test_balance_due_after_partial_payment(self):
        self.account.pay(Money(60000))
        self.assertEqual(self.account.balance_due(), Money(50000))
        self.assertFalse(self.account.paid_in_full())

    def test_paid_in_full(self):
        self.account.pay(Money(110000))
        self.assertTrue(self.account.paid_in_full())

    def test_report_summary(self):
        self.assertEqual(summary_line([self.account.invoice], 1000), "1 invoices, $1100.00")


if __name__ == "__main__":
    unittest.main()
