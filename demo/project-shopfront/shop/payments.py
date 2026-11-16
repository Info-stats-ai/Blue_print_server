"""Taking money and giving it back. Every movement is kept in a ledger."""
from dataclasses import dataclass

from shop.money import Money, ZERO, total


class PaymentDeclined(Exception):
    pass


@dataclass(frozen=True)
class Transaction:
    order_id: str
    kind: str  # "charge" or "refund"
    amount: Money
    card_last4: str


class PaymentGateway:
    """Stands in for a card processor. Cards ending 0000 are always declined."""

    def __init__(self):
        self.ledger = []

    def charge(self, order_id: str, amount: Money, card_last4: str) -> Transaction:
        if amount.cents <= 0:
            raise ValueError("charge more than zero")
        if card_last4 == "0000":
            raise PaymentDeclined(f"card ending {card_last4} was declined")
        transaction = Transaction(order_id, "charge", amount, card_last4)
        self.ledger.append(transaction)
        return transaction

    def refund(self, order_id: str, amount: Money) -> Transaction:
        """Gives money back to the card that paid, up to what's left of the charge."""
        if amount.cents <= 0:
            raise ValueError("refund more than zero")
        if amount > self.refundable(order_id):
            raise ValueError("can't refund more than was paid")
        card = next(t.card_last4 for t in self.ledger if t.order_id == order_id and t.kind == "charge")
        transaction = Transaction(order_id, "refund", amount, card)
        self.ledger.append(transaction)
        return transaction

    def paid(self, order_id: str) -> Money:
        return total(t.amount for t in self.ledger if t.order_id == order_id and t.kind == "charge")

    def refunded(self, order_id: str) -> Money:
        return total(t.amount for t in self.ledger if t.order_id == order_id and t.kind == "refund")

    def refundable(self, order_id: str) -> Money:
        return self.paid(order_id) - self.refunded(order_id)

    def net_revenue(self) -> Money:
        charges = total(t.amount for t in self.ledger if t.kind == "charge")
        refunds = total(t.amount for t in self.ledger if t.kind == "refund")
        return charges - refunds if self.ledger else ZERO
