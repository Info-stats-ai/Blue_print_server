"""Numbers for whoever runs the store."""
from collections import Counter

from shop.money import Money, ZERO, format_money, total
from shop.orders import OrderBook
from shop.payments import PaymentGateway

COUNTED = ("paid", "shipped", "delivered")


def sales_by_day(orders: OrderBook):
    days = {}
    for order in orders.all():
        if order.status in COUNTED:
            days[order.placed_on] = days.get(order.placed_on, ZERO) + order.quote.total
    return dict(sorted(days.items()))


def top_products(orders: OrderBook, limit: int = 3):
    sold = Counter()
    for order in orders.all():
        if order.status in COUNTED:
            sold.update(order.lines)
    return sorted(sold.items(), key=lambda item: (-item[1], item[0]))[:limit]


def average_order_value(orders: OrderBook) -> Money:
    counted = [order.quote.total for order in orders.all() if order.status in COUNTED]
    return Money(total(counted).cents // len(counted)) if counted else ZERO


def refund_rate_bp(payments: PaymentGateway) -> int:
    """Refunds as a share of charges, in basis points."""
    charged = total(t.amount for t in payments.ledger if t.kind == "charge")
    refunded = total(t.amount for t in payments.ledger if t.kind == "refund")
    return refunded.cents * 10_000 // charged.cents if charged.cents else 0


def summary(orders: OrderBook, payments: PaymentGateway) -> str:
    return "\n".join([
        f"Orders counted: {sum(1 for order in orders.all() if order.status in COUNTED)}",
        f"Net revenue: {format_money(payments.net_revenue())}",
        f"Average order: {format_money(average_order_value(orders))}",
        f"Refund rate: {refund_rate_bp(payments) / 100:.2f}%",
    ])
