"""Sending things back: the stock returns to the shelf and the money to the card."""
from dataclasses import dataclass

from shop.catalog import Catalog
from shop.inventory import Inventory
from shop.money import Money, total
from shop.orders import Order
from shop.payments import PaymentGateway

RETURN_WINDOW_DAYS = 30


class ReturnRefused(Exception):
    pass


@dataclass(frozen=True)
class ReturnReceipt:
    order_id: str
    lines: dict
    refunded: Money


def refund_for(order: Order, lines: dict, catalog: Catalog) -> Money:
    """The share of what was paid that the returned items make up. Shipping
    isn't refunded; discount and tax are given back in proportion."""
    returned_value = total(catalog.price_of(sku).times(quantity) for sku, quantity in lines.items())
    goods_paid = order.quote.subtotal - order.quote.discount + order.quote.tax
    if order.quote.subtotal.cents == 0:
        return Money(0)
    return Money((goods_paid.cents * returned_value.cents + order.quote.subtotal.cents // 2) // order.quote.subtotal.cents)


def process_return(order: Order, lines: dict, today, catalog: Catalog, inventory: Inventory, payments: PaymentGateway) -> ReturnReceipt:
    if order.status != "delivered":
        raise ReturnRefused("only delivered orders can be returned")
    if (today - order.placed_on).days > RETURN_WINDOW_DAYS:
        raise ReturnRefused(f"returns close after {RETURN_WINDOW_DAYS} days")
    for sku, quantity in lines.items():
        if quantity <= 0 or quantity > order.lines.get(sku, 0):
            raise ReturnRefused(f"{sku} x {quantity} wasn't in the order")
    amount = min(refund_for(order, lines, catalog), payments.refundable(order.order_id))
    if amount.cents > 0:
        payments.refund(order.order_id, amount)
    for sku, quantity in lines.items():
        inventory.restock(sku, quantity)
    if lines == order.lines:
        order.move_to("refunded")
    return ReturnReceipt(order.order_id, dict(lines), amount)
