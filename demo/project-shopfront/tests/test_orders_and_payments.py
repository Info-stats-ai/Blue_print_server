import unittest

from shop.inventory import OutOfStock
from shop.money import Money
from shop.orders import InvalidTransition
from shop.payments import PaymentDeclined, PaymentGateway
from shop.pricing import quote
from tests.helpers import TODAY, mugs_and_lamp, stocked_store


class OrderBookTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()
        self.cart = mugs_and_lamp(self.store)
        self.quote = quote(self.cart, self.store.customers.get("c1"), self.store.coupons, TODAY)

    def test_placing_reserves_stock(self):
        order = self.store.orders.place("c1", self.cart.lines, self.quote, TODAY)
        self.assertEqual(order.order_id, "ORD-00001")
        self.assertEqual((self.store.inventory.available("MUG"), self.store.inventory.available("LAMP")), (8, 2))

    def test_all_lines_or_none(self):
        self.cart.set_quantity("LAMP", 4)  # only 3 in stock
        with self.assertRaises(OutOfStock):
            self.store.orders.place("c1", self.cart.lines, self.quote, TODAY)
        self.assertEqual(self.store.inventory.available("MUG"), 10, "the mugs held first were given back")

    def test_cancel_gives_stock_back(self):
        order = self.store.orders.place("c1", self.cart.lines, self.quote, TODAY)
        self.store.orders.cancel(order.order_id)
        self.assertEqual(self.store.inventory.available("LAMP"), 3)
        self.assertFalse(order.is_open())

    def test_statuses_only_move_forward(self):
        order = self.store.orders.place("c1", self.cart.lines, self.quote, TODAY)
        with self.assertRaises(InvalidTransition):
            order.move_to("delivered")
        order.move_to("paid")
        order.move_to("shipped")
        with self.assertRaises(InvalidTransition):
            order.move_to("cancelled")
        self.assertEqual(order.history, ["placed", "paid", "shipped"])

    def test_an_empty_order_is_refused(self):
        with self.assertRaises(ValueError):
            self.store.orders.place("c1", {}, self.quote, TODAY)


class PaymentGatewayTest(unittest.TestCase):
    def setUp(self):
        self.payments = PaymentGateway()

    def test_charge_and_partial_refunds(self):
        self.payments.charge("ORD-1", Money(8_268), "4242")
        self.payments.refund("ORD-1", Money(3_000))
        self.assertEqual(self.payments.refundable("ORD-1"), Money(5_268))
        self.assertEqual(self.payments.net_revenue(), Money(5_268))
        self.assertEqual(self.payments.ledger[-1].card_last4, "4242", "refunds go back to the card that paid")

    def test_cant_refund_more_than_was_paid(self):
        self.payments.charge("ORD-1", Money(1_000), "4242")
        with self.assertRaises(ValueError):
            self.payments.refund("ORD-1", Money(1_001))
        with self.assertRaises(ValueError):
            self.payments.refund("ORD-2", Money(1))

    def test_declined_card_leaves_no_trace(self):
        with self.assertRaises(PaymentDeclined):
            self.payments.charge("ORD-1", Money(1_000), "0000")
        self.assertEqual(self.payments.ledger, [])

    def test_charges_must_be_positive(self):
        with self.assertRaises(ValueError):
            self.payments.charge("ORD-1", Money(0), "4242")


if __name__ == "__main__":
    unittest.main()
