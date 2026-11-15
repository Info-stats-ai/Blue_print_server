"""Invoices: line items, subtotal, tax and total."""
from ledger.money import Money


class Invoice:
    def __init__(self, number: str, customer: str):
        self.number = number
        self.customer = customer
        self.lines = []

    def add_line(self, description: str, unit_price: Money, quantity: int = 1):
        self.lines.append((description, unit_price, quantity))

    def subtotal(self) -> Money:
        total = Money(0)
        for _, unit_price, quantity in self.lines:
            total = total + unit_price.times(quantity)
        return total

    def tax(self, rate_bp: int) -> Money:
        # Basis points, rounded half up.
        return Money((self.subtotal().cents * rate_bp + 5000) // 10000)

    def total(self, rate_bp: int) -> Money:
        return self.subtotal() + self.tax(rate_bp)
