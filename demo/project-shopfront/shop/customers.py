"""Who buys, and how loyal they are."""
from dataclasses import dataclass, field

from shop.money import Money, ZERO


@dataclass
class Customer:
    customer_id: str
    name: str
    email: str
    zone: str = "domestic"
    lifetime_spend: Money = field(default=ZERO)

    def tier(self) -> str:
        if self.lifetime_spend >= Money(100_000):
            return "gold"
        if self.lifetime_spend >= Money(25_000):
            return "silver"
        return "standard"

    def record_purchase(self, amount: Money) -> None:
        self.lifetime_spend = self.lifetime_spend + amount


class CustomerDirectory:
    def __init__(self):
        self._customers = {}

    def add(self, customer: Customer) -> None:
        if "@" not in customer.email:
            raise ValueError("that doesn't look like an email address")
        self._customers[customer.customer_id] = customer

    def get(self, customer_id: str) -> Customer:
        return self._customers[customer_id]

    def by_email(self, email: str):
        email = email.strip().lower()
        return next((c for c in self._customers.values() if c.email.lower() == email), None)
