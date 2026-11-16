"""What the store sells."""
from dataclasses import dataclass

from shop.money import Money


@dataclass(frozen=True)
class Product:
    sku: str
    name: str
    price: Money
    weight_grams: int
    category: str = "general"


class UnknownProduct(KeyError):
    pass


class Catalog:
    def __init__(self):
        self._products = {}

    def add(self, product: Product) -> None:
        if product.price.is_negative():
            raise ValueError("a price can't be negative")
        self._products[product.sku] = product

    def get(self, sku: str) -> Product:
        try:
            return self._products[sku]
        except KeyError:
            raise UnknownProduct(sku) from None

    def price_of(self, sku: str) -> Money:
        return self.get(sku).price

    def search(self, text: str):
        text = text.lower()
        return sorted(
            (product for product in self._products.values() if text in product.name.lower()),
            key=lambda product: product.name,
        )

    def in_category(self, category: str):
        return sorted(
            (product for product in self._products.values() if product.category == category),
            key=lambda product: product.sku,
        )
