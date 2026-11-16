import unittest

from shop.catalog import Product, UnknownProduct
from shop.money import Money
from tests.helpers import stocked_store


class CatalogTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()

    def test_finds_products_by_name_and_category(self):
        self.assertEqual([p.sku for p in self.store.catalog.search("desk")], ["LAMP", "DESK"])
        self.assertEqual([p.sku for p in self.store.catalog.in_category("home")], ["DESK", "LAMP"])

    def test_unknown_product(self):
        with self.assertRaises(UnknownProduct):
            self.store.catalog.get("SOFA")

    def test_refuses_a_negative_price(self):
        with self.assertRaises(ValueError):
            self.store.catalog.add(Product("BAD", "Bad", Money(-1), 10))


class CartTest(unittest.TestCase):
    def setUp(self):
        self.cart = stocked_store().new_cart()

    def test_subtotal_and_weight(self):
        self.cart.add("MUG", 2)
        self.cart.add("LAMP")
        self.assertEqual(self.cart.subtotal(), Money(6_900))
        self.assertEqual(self.cart.weight_grams(), 2_100)
        self.assertEqual(self.cart.item_count(), 3)

    def test_adding_twice_adds_up(self):
        self.cart.add("PEN", 2)
        self.cart.add("PEN", 3)
        self.assertEqual(self.cart.lines, {"PEN": 5})

    def test_quantity_zero_removes_the_line(self):
        self.cart.add("PEN", 2)
        self.cart.set_quantity("PEN", 0)
        self.assertTrue(self.cart.is_empty())

    def test_cant_add_what_the_store_doesnt_sell(self):
        with self.assertRaises(UnknownProduct):
            self.cart.add("SOFA")
        with self.assertRaises(ValueError):
            self.cart.add("PEN", 0)


if __name__ == "__main__":
    unittest.main()
