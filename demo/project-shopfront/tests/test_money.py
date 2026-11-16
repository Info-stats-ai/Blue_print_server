import unittest

from shop.money import Money, ZERO, format_money, total


class MoneyTest(unittest.TestCase):
    def test_adds_subtracts_and_multiplies(self):
        self.assertEqual(Money(1_200) + Money(250), Money(1_450))
        self.assertEqual(Money(1_200) - Money(250), Money(950))
        self.assertEqual(Money(1_200).times(3), Money(3_600))

    def test_percent_rounds_half_up(self):
        self.assertEqual(Money(1_000).percent(825), Money(83))  # 82.5
        self.assertEqual(Money(999).percent(825), Money(82))  # 82.4175
        self.assertEqual(Money(1_006).percent(825), Money(83))  # 82.995

    def test_total_of_nothing_is_zero(self):
        self.assertEqual(total([]), ZERO)
        self.assertEqual(total([Money(1), Money(2), Money(3)]), Money(6))

    def test_only_whole_cents(self):
        with self.assertRaises(TypeError):
            Money(9.99)

    def test_formats_like_a_price_tag(self):
        self.assertEqual(format_money(Money(123_456)), "$1,234.56")
        self.assertEqual(format_money(Money(5)), "$0.05")
        self.assertEqual(format_money(Money(-5)), "-$0.05")


if __name__ == "__main__":
    unittest.main()
