import unittest

from shop.inventory import Inventory, OutOfStock


class InventoryTest(unittest.TestCase):
    def setUp(self):
        self.inventory = Inventory()
        self.inventory.restock("MUG", 10)

    def test_reserving_holds_stock_without_removing_it(self):
        self.inventory.reserve("MUG", 4)
        self.assertEqual(self.inventory.available("MUG"), 6)

    def test_cant_reserve_more_than_is_available(self):
        self.inventory.reserve("MUG", 4)
        with self.assertRaises(OutOfStock) as caught:
            self.inventory.reserve("MUG", 7)
        self.assertEqual((caught.exception.wanted, caught.exception.available), (7, 6))
        self.assertEqual(self.inventory.available("MUG"), 6)

    def test_release_gives_it_back(self):
        self.inventory.reserve("MUG", 4)
        self.inventory.release("MUG", 2)
        self.assertEqual(self.inventory.available("MUG"), 8)

    def test_shipping_takes_it_off_the_shelf(self):
        self.inventory.reserve("MUG", 4)
        self.inventory.ship("MUG", 4)
        self.assertEqual(self.inventory.available("MUG"), 6)
        with self.assertRaises(ValueError):
            self.inventory.ship("MUG", 1)

    def test_low_stock_list(self):
        self.inventory.restock("LAMP", 2)
        self.inventory.reserve("MUG", 6)
        self.assertEqual(self.inventory.low_stock(), ["LAMP", "MUG"])
        self.assertEqual(self.inventory.low_stock(threshold=3), ["LAMP"])


if __name__ == "__main__":
    unittest.main()
