"""What a customer is about to buy."""
from shop.catalog import Catalog
from shop.money import Money, total


class Cart:
    def __init__(self, catalog: Catalog):
        self.catalog = catalog
        self.lines = {}
        self.coupon_code = None

    def add(self, sku: str, quantity: int = 1) -> None:
        if quantity <= 0:
            raise ValueError("add at least one")
        self.catalog.get(sku)
        self.lines[sku] = self.lines.get(sku, 0) + quantity

    def remove(self, sku: str) -> None:
        self.lines.pop(sku, None)

    def set_quantity(self, sku: str, quantity: int) -> None:
        if quantity <= 0:
            self.remove(sku)
        else:
            self.catalog.get(sku)
            self.lines[sku] = quantity

    def is_empty(self) -> bool:
        return not self.lines

    def item_count(self) -> int:
        return sum(self.lines.values())

    def subtotal(self) -> Money:
        return total(self.catalog.price_of(sku).times(quantity) for sku, quantity in self.lines.items())

    def weight_grams(self) -> int:
        return sum(self.catalog.get(sku).weight_grams * quantity for sku, quantity in self.lines.items())

    def apply_coupon(self, code: str) -> None:
        self.coupon_code = code.strip().upper() or None
