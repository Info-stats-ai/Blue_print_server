"""Payments against an invoice."""
from ledger.invoice import Invoice
from ledger.money import Money


class Account:
    def __init__(self, invoice: Invoice, rate_bp: int):
        self.invoice = invoice
        self.rate_bp = rate_bp
        self.payments = []

    def pay(self, amount: Money):
        self.payments.append(amount)

    def balance_due(self) -> Money:
        paid = Money(0)
        for amount in self.payments:
            paid = paid + amount
        return self.invoice.total(self.rate_bp) - paid

    def paid_in_full(self) -> bool:
        return self.balance_due().cents <= 0
