import unittest
from datetime import date

from shop.coupons import Coupon
from shop.money import Money, ZERO
from shop.pricing import quote
from shop.shipping import UnknownZone, shipping_cost
from tests.helpers import TODAY, mugs_and_lamp, stocked_store


class ShippingTest(unittest.TestCase):
    def test_first_kilo_then_each_started_kilo(self):
        self.assertEqual(shipping_cost(350, "domestic", Money(1_200)), Money(499))
        self.assertEqual(shipping_cost(1_000, "domestic", Money(1_200)), Money(499))
        self.assertEqual(shipping_cost(1_001, "domestic", Money(1_200)), Money(649))
        self.assertEqual(shipping_cost(2_750, "domestic", Money(3_000)), Money(799))

    def test_free_at_home_over_the_line_never_abroad(self):
        self.assertEqual(shipping_cost(5_000, "domestic", Money(7_500)), ZERO)
        self.assertEqual(shipping_cost(1_400, "international", Money(9_000)), Money(2_499))

    def test_unknown_zone(self):
        with self.assertRaises(UnknownZone):
            shipping_cost(100, "moon", Money(100))


class QuoteTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()
        self.ada = self.store.customers.get("c1")
        self.cart = mugs_and_lamp(self.store)

    def quote(self):
        return quote(self.cart, self.ada, self.store.coupons, TODAY)

    def test_plain_order(self):
        q = self.quote()
        self.assertEqual((q.subtotal, q.discount, q.tax, q.shipping), (Money(6_900), ZERO, Money(569), Money(799)))
        self.assertEqual(q.total, Money(8_268))

    def test_coupon_comes_off_before_tax(self):
        self.cart.apply_coupon("save10")
        q = self.quote()
        self.assertEqual((q.discount, q.tax), (Money(690), Money(512)))
        self.assertEqual(q.total, Money(7_521))

    def test_coupon_below_its_minimum_spend_does_nothing(self):
        cart = self.store.new_cart()
        cart.add("MUG")
        cart.apply_coupon("SAVE10")
        self.assertEqual(quote(cart, self.ada, self.store.coupons, TODAY).discount, ZERO)

    def test_expired_coupon_does_nothing(self):
        self.cart.apply_coupon("OLD")
        self.assertEqual(self.quote().discount, ZERO)
        self.assertEqual(quote(self.cart, self.ada, self.store.coupons, date(2026, 1, 31)).discount, Money(3_450))

    def test_loyalty_and_coupon_dont_stack_the_better_one_wins(self):
        self.ada.record_purchase(Money(100_000))
        self.assertEqual(self.ada.tier(), "gold")
        self.assertEqual(self.quote().discount, Money(483))
        self.assertEqual(self.quote().tax, Money(529))
        self.cart.apply_coupon("SAVE10")
        self.assertEqual(self.quote().discount, Money(690))

    def test_a_coupon_never_takes_more_than_the_subtotal(self):
        greedy = Coupon("ALL", percent_off_bp=10_000, amount_off=Money(500))
        self.assertEqual(greedy.discount_for(Money(250), TODAY), Money(250))

    def test_international_order(self):
        cart = self.store.new_cart()
        cart.add("PEN")
        q = quote(cart, self.store.customers.get("c2"), self.store.coupons, TODAY)
        self.assertEqual((q.tax, q.shipping, q.total), (Money(21), Money(1_899), Money(2_170)))


if __name__ == "__main__":
    unittest.main()
