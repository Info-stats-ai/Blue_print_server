import unittest

from ledger.invoice import Invoice
from ledger.money import Money, format_money


class InvoiceTests(unittest.TestCase):
    def setUp(self):
        self.invoice = Invoice("INV-001", "Acme")
        self.invoice.add_line("Design", Money(120000))
        self.invoice.add_line("Hosting", Money(2500), quantity=4)

    def test_subtotal(self):
        self.assertEqual(self.invoice.subtotal(), Money(130000))

    def test_tax_rounds_half_up(self):
        self.assertEqual(self.invoice.tax(825), Money(10725))

    def test_total(self):
        self.assertEqual(self.invoice.total(825), Money(140725))
        self.assertEqual(format_money(self.invoice.total(825)), "$1407.25")


if __name__ == "__main__":
    unittest.main()
