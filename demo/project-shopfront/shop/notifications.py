"""Messages the store sends customers. They go to an outbox, not the network."""
from dataclasses import dataclass

from shop.customers import Customer
from shop.money import format_money
from shop.orders import Order


@dataclass(frozen=True)
class Message:
    to: str
    subject: str
    body: str


class Outbox:
    def __init__(self):
        self.messages = []

    def send(self, message: Message) -> None:
        self.messages.append(message)

    def to(self, email: str):
        return [message for message in self.messages if message.to == email]


def order_confirmation(customer: Customer, order: Order) -> Message:
    items = ", ".join(f"{quantity} x {sku}" for sku, quantity in sorted(order.lines.items()))
    return Message(
        customer.email,
        f"Order {order.order_id} confirmed",
        f"Hi {customer.name}, we got your order ({items}). Total: {format_money(order.quote.total)}.",
    )


def shipped_notice(customer: Customer, order: Order, tracking: str) -> Message:
    return Message(
        customer.email,
        f"Order {order.order_id} is on its way",
        f"Hi {customer.name}, your order has shipped. Track it with {tracking}.",
    )


def refund_notice(customer: Customer, order: Order, amount) -> Message:
    return Message(
        customer.email,
        f"Refund for order {order.order_id}",
        f"Hi {customer.name}, we refunded {format_money(amount)} to your card.",
    )
