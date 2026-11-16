"""The store end to end: what a customer and the people running it would do."""
import unittest
from datetime import timedelta

from shop.money import Money
from shop.payments import PaymentDeclined
from shop.returns import ReturnRefused
from tests.helpers import TODAY, mugs_and_lamp, stocked_store


class CheckoutTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()

    def test_checkout_charges_reserves_and_confirms(self):
        order = self.store.checkout("c1", mugs_and_lamp(self.store), "4242", TODAY)
        self.assertEqual((order.status, order.quote.total), ("paid", Money(8_268)))
        self.assertEqual(self.store.payments.paid(order.order_id), Money(8_268))
        self.assertEqual(self.store.inventory.available("MUG"), 8)
        self.assertEqual(self.store.customers.get("c1").lifetime_spend, Money(8_268))
        [message] = self.store.outbox.to("ada@example.test")
        self.assertIn("Total: $82.68", message.body)
        self.assertIn("1 x LAMP, 2 x MUG", message.body)

    def test_declined_card_cancels_and_gives_stock_back(self):
        with self.assertRaises(PaymentDeclined):
            self.store.checkout("c1", mugs_and_lamp(self.store), "0000", TODAY)
        self.assertEqual((self.store.inventory.available("MUG"), self.store.inventory.available("LAMP")), (10, 3))
        self.assertEqual([order.status for order in self.store.orders.all()], ["cancelled"])
        self.assertEqual(self.store.outbox.messages, [])

    def test_empty_cart(self):
        with self.assertRaises(ValueError):
            self.store.checkout("c1", self.store.new_cart(), "4242", TODAY)

    def test_the_last_desk_can_only_be_sold_once(self):
        first, second = self.store.new_cart(), self.store.new_cart()
        first.add("DESK")
        second.add("DESK")
        self.store.checkout("c1", first, "4242", TODAY)
        with self.assertRaises(Exception) as caught:
            self.store.checkout("c2", second, "4242", TODAY)
        self.assertEqual(type(caught.exception).__name__, "OutOfStock")
        self.assertEqual(self.store.payments.paid("ORD-00002"), Money(0), "nobody was charged for it")


class AfterCheckoutTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()
        self.order = self.store.checkout("c1", mugs_and_lamp(self.store), "4242", TODAY)

    def test_ship_then_deliver(self):
        tracking = self.store.ship(self.order.order_id)
        self.assertEqual(tracking, "TRK000001")
        self.assertIn(tracking, self.store.outbox.to("ada@example.test")[-1].body)
        self.store.deliver(self.order.order_id)
        self.assertEqual(self.order.status, "delivered")
        self.assertEqual(self.store.shipping.for_order(self.order.order_id).status(), "delivered")

    def test_cancel_before_shipping_refunds_everything(self):
        self.store.cancel(self.order.order_id)
        self.assertEqual(self.store.payments.refundable(self.order.order_id), Money(0))
        self.assertEqual(self.store.inventory.available("LAMP"), 3)
        self.assertIn("refunded $82.68", self.store.outbox.to("ada@example.test")[-1].body)

    def test_return_one_item_refunds_its_share_not_the_shipping(self):
        self.store.ship(self.order.order_id)
        self.store.deliver(self.order.order_id)
        receipt = self.store.return_items(self.order.order_id, {"LAMP": 1}, TODAY + timedelta(days=10))
        self.assertEqual(receipt.refunded, Money(4_871))
        self.assertEqual(self.store.inventory.available("LAMP"), 3, "back on the shelf")
        self.assertEqual(self.order.status, "delivered", "the mugs were kept")
        rest = self.store.return_items(self.order.order_id, {"MUG": 2}, TODAY + timedelta(days=12))
        self.assertEqual(receipt.refunded + rest.refunded, Money(7_469), "everything but the $7.99 shipping")

    def test_returning_everything_marks_it_refunded(self):
        self.store.ship(self.order.order_id)
        self.store.deliver(self.order.order_id)
        receipt = self.store.return_items(self.order.order_id, {"MUG": 2, "LAMP": 1}, TODAY + timedelta(days=3))
        self.assertEqual((receipt.refunded, self.order.status), (Money(7_469), "refunded"))

    def test_returns_have_rules(self):
        with self.assertRaises(ReturnRefused):
            self.store.return_items(self.order.order_id, {"LAMP": 1}, TODAY)  # not delivered yet
        self.store.ship(self.order.order_id)
        self.store.deliver(self.order.order_id)
        with self.assertRaises(ReturnRefused):
            self.store.return_items(self.order.order_id, {"LAMP": 2}, TODAY)  # only bought one
        with self.assertRaises(ReturnRefused):
            self.store.return_items(self.order.order_id, {"LAMP": 1}, TODAY + timedelta(days=31))
        self.assertEqual(self.store.payments.refunded(self.order.order_id), Money(0))


if __name__ == "__main__":
    unittest.main()
