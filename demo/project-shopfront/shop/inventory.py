"""How many of each product are on the shelf, and how many are promised."""


class OutOfStock(Exception):
    def __init__(self, sku: str, wanted: int, available: int):
        super().__init__(f"{sku}: wanted {wanted}, only {available} available")
        self.sku, self.wanted, self.available = sku, wanted, available


class Inventory:
    def __init__(self):
        self._on_hand = {}
        self._reserved = {}

    def restock(self, sku: str, quantity: int) -> None:
        if quantity <= 0:
            raise ValueError("restock at least one")
        self._on_hand[sku] = self._on_hand.get(sku, 0) + quantity

    def available(self, sku: str) -> int:
        return self._on_hand.get(sku, 0) - self._reserved.get(sku, 0)

    def reserve(self, sku: str, quantity: int) -> None:
        """Promises stock to an order. It stays on the shelf until shipped."""
        available = self.available(sku)
        if quantity > available:
            raise OutOfStock(sku, quantity, available)
        self._reserved[sku] = self._reserved.get(sku, 0) + quantity

    def release(self, sku: str, quantity: int) -> None:
        """Gives promised stock back, when an order is cancelled."""
        self._reserved[sku] = max(0, self._reserved.get(sku, 0) - quantity)

    def ship(self, sku: str, quantity: int) -> None:
        """Promised stock leaves the shelf."""
        if quantity > self._reserved.get(sku, 0):
            raise ValueError(f"{sku}: can't ship more than was reserved")
        self._reserved[sku] -= quantity
        self._on_hand[sku] -= quantity

    def low_stock(self, threshold: int = 5):
        return sorted(sku for sku in self._on_hand if self.available(sku) <= threshold)
