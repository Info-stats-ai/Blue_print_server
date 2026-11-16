"""A small stocked store every test starts from."""
from datetime import date

from shop.catalog import Product
from shop.coupons import Coupon
from shop.customers import Customer
from shop.money import Money
from shop.store import Store

TODAY = date(2026, 3, 2)


def stocked_store() -> Store:
    store = Store()
    for product, stock in [
        (Product("MUG", "Stoneware mug", Money(1_200), 350, "kitchen"), 10),
        (Product("LAMP", "Desk lamp", Money(4_500), 1_400, "home"), 3),
        (Product("PEN", "Gel pen", Money(250), 20, "office"), 100),
        (Product("DESK", "Standing desk", Money(32_000), 28_000, "home"), 1),
    ]:
        store.catalog.add(product)
        store.inventory.restock(product.sku, stock)
    store.customers.add(Customer("c1", "Ada", "ada@example.test"))
    store.customers.add(Customer("c2", "Lin", "lin@example.test", zone="international"))
    store.coupons.add(Coupon("SAVE10", percent_off_bp=1_000, minimum_spend=Money(5_000)))
    store.coupons.add(Coupon("FIVER", amount_off=Money(500)))
    store.coupons.add(Coupon("OLD", percent_off_bp=5_000, expires=date(2026, 1, 31)))
    return store


def mugs_and_lamp(store: Store):
    cart = store.new_cart()
    cart.add("MUG", 2)
    cart.add("LAMP")
    return cart
