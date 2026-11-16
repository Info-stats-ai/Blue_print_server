"""Turns a cart into what the customer pays."""
from dataclasses import dataclass
from datetime import date

from shop.cart import Cart
from shop.coupons import CouponBook
from shop.customers import Customer
from shop.money import Money, ZERO
from shop.shipping import shipping_cost

TAX_RATE_BP = 825
TIER_DISCOUNT_BP = {"standard": 0, "silver": 300, "gold": 700}


@dataclass(frozen=True)
class Quote:
    subtotal: Money
    discount: Money
    tax: Money
    shipping: Money

    @property
    def total(self) -> Money:
        return self.subtotal - self.discount + self.tax + self.shipping


def loyalty_discount(customer: Customer, subtotal: Money) -> Money:
    return subtotal.percent(TIER_DISCOUNT_BP[customer.tier()])


def quote(cart: Cart, customer: Customer, coupons: CouponBook, day: date) -> Quote:
    """Subtotal, less the better of the coupon and the loyalty discount (they
    don't stack), plus tax on what's left, plus shipping."""
    subtotal = cart.subtotal()
    coupon = coupons.find(cart.coupon_code) if cart.coupon_code else None
    coupon_discount = coupon.discount_for(subtotal, day) if coupon else ZERO
    discount = max(coupon_discount, loyalty_discount(customer, subtotal))
    taxable = subtotal - discount
    tax = taxable.percent(TAX_RATE_BP)
    shipping = shipping_cost(cart.weight_grams(), customer.zone, subtotal)
    return Quote(subtotal, discount, tax, shipping)
