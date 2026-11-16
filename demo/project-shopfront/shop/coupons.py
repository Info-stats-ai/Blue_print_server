"""Discount codes."""
from dataclasses import dataclass
from datetime import date

from shop.money import Money, ZERO


@dataclass(frozen=True)
class Coupon:
    code: str
    percent_off_bp: int = 0
    amount_off: Money = ZERO
    minimum_spend: Money = ZERO
    expires: date = None

    def is_valid_on(self, day: date) -> bool:
        return self.expires is None or day <= self.expires

    def discount_for(self, subtotal: Money, day: date) -> Money:
        """What this coupon takes off `subtotal`; never more than the subtotal."""
        if not self.is_valid_on(day) or subtotal < self.minimum_spend:
            return ZERO
        discount = subtotal.percent(self.percent_off_bp) + self.amount_off
        return min(discount, subtotal)


class CouponBook:
    def __init__(self):
        self._coupons = {}

    def add(self, coupon: Coupon) -> None:
        self._coupons[coupon.code.upper()] = coupon

    def find(self, code: str):
        return self._coupons.get(code.strip().upper())
