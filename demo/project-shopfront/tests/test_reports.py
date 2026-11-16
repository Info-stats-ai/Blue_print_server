import unittest
from datetime import timedelta

from shop import reports
from shop.money import Money
from tests.helpers import TODAY, mugs_and_lamp, stocked_store


class ReportsTest(unittest.TestCase):
    def setUp(self):
        self.store = stocked_store()
        self.first = self.store.checkout("c1", mugs_and_lamp(self.store), "4242", TODAY)
        pens = self.store.new_cart()
        pens.add("PEN", 4)
        self.second = self.store.checkout("c2", pens, "1881", TODAY + timedelta(days=1))
        cancelled = self.store.new_cart()
        cancelled.add("MUG")
        self.store.cancel(self.store.checkout("c1", cancelled, "4242", TODAY).order_id)

    def test_sales_by_day_skip_cancelled_orders(self):
        self.assertEqual(
            reports.sales_by_day(self.store.orders),
            {TODAY: Money(8_268), TODAY + timedelta(days=1): Money(2_982)},
        )

    def test_top_products(self):
        self.assertEqual(reports.top_products(self.store.orders, limit=2), [("PEN", 4), ("MUG", 2)])

    def test_average_order_value(self):
        self.assertEqual(reports.average_order_value(self.store.orders), Money(5_625))

    def test_refund_rate(self):
        # Charged 8,268 + 2,982 + 1,798 = 13,048; the cancelled 1,798 was
        # refunded: 13.7799...%, cut to whole basis points.
        self.assertEqual(reports.refund_rate_bp(self.store.payments), 1_377)

    def test_summary_reads_like_a_note_to_the_owner(self):
        self.assertEqual(
            reports.summary(self.store.orders, self.store.payments),
            "Orders counted: 2\nNet revenue: $112.50\nAverage order: $56.25\nRefund rate: 13.77%",
        )


if __name__ == "__main__":
    unittest.main()
