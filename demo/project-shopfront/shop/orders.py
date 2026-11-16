"""An order from the moment it's placed to the moment it's delivered."""
from dataclasses import dataclass, field
from datetime import date

from shop.inventory import Inventory
from shop.pricing import Quote

NEXT_STATUS = {
    "placed": {"paid", "cancelled"},
    "paid": {"shipped", "cancelled", "refunded"},
    "shipped": {"delivered", "refunded"},
    "delivered": {"refunded"},
    "cancelled": set(),
    "refunded": set(),
}


class InvalidTransition(Exception):
    pass


@dataclass
class Order:
    order_id: str
    customer_id: str
    lines: dict
    quote: Quote
    placed_on: date
    status: str = "placed"
    history: list = field(default_factory=lambda: ["placed"])

    def move_to(self, status: str) -> None:
        if status not in NEXT_STATUS[self.status]:
            raise InvalidTransition(f"an order that is {self.status} can't become {status}")
        self.status = status
        self.history.append(status)

    def is_open(self) -> bool:
        return self.status in ("placed", "paid", "shipped")


class OrderBook:
    def __init__(self, inventory: Inventory):
        self.inventory = inventory
        self._orders = {}
        self._next = 1

    def place(self, customer_id: str, lines: dict, quote: Quote, placed_on: date) -> Order:
        """Reserves every line or none: a failed line gives back what was held."""
        if not lines:
            raise ValueError("an order needs at least one item")
        reserved = []
        try:
            for sku, quantity in lines.items():
                self.inventory.reserve(sku, quantity)
                reserved.append((sku, quantity))
        except Exception:
            for sku, quantity in reserved:
                self.inventory.release(sku, quantity)
            raise
        order = Order(f"ORD-{self._next:05d}", customer_id, dict(lines), quote, placed_on)
        self._next += 1
        self._orders[order.order_id] = order
        return order

    def get(self, order_id: str) -> Order:
        return self._orders[order_id]

    def cancel(self, order_id: str) -> Order:
        order = self.get(order_id)
        order.move_to("cancelled")
        for sku, quantity in order.lines.items():
            self.inventory.release(sku, quantity)
        return order

    def ship(self, order_id: str) -> Order:
        order = self.get(order_id)
        order.move_to("shipped")
        for sku, quantity in order.lines.items():
            self.inventory.ship(sku, quantity)
        return order

    def for_customer(self, customer_id: str):
        return [order for order in self._orders.values() if order.customer_id == customer_id]

    def all(self):
        return list(self._orders.values())
