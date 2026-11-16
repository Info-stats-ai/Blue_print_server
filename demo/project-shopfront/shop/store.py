"""The whole store in one place: checkout, shipping, cancelling and returns."""
from datetime import date

from shop.cart import Cart
from shop.catalog import Catalog
from shop.coupons import CouponBook
from shop.customers import CustomerDirectory
from shop.inventory import Inventory
from shop.notifications import Outbox, order_confirmation, refund_notice, shipped_notice
from shop.orders import Order, OrderBook
from shop.payments import PaymentDeclined, PaymentGateway
from shop.pricing import quote
from shop.returns import ReturnReceipt, process_return
from shop.shipping import ShippingDesk


class Store:
    def __init__(self):
        self.catalog = Catalog()
        self.inventory = Inventory()
        self.coupons = CouponBook()
        self.customers = CustomerDirectory()
        self.orders = OrderBook(self.inventory)
        self.payments = PaymentGateway()
        self.shipping = ShippingDesk()
        self.outbox = Outbox()

    def new_cart(self) -> Cart:
        return Cart(self.catalog)

    def checkout(self, customer_id: str, cart: Cart, card_last4: str, today: date) -> Order:
        """Prices the cart, holds the stock, takes the money. A declined card
        cancels the order and gives the stock back."""
        if cart.is_empty():
            raise ValueError("the cart is empty")
        customer = self.customers.get(customer_id)
        order = self.orders.place(customer_id, cart.lines, quote(cart, customer, self.coupons, today), today)
        try:
            self.payments.charge(order.order_id, order.quote.total, card_last4)
        except PaymentDeclined:
            self.orders.cancel(order.order_id)
            raise
        order.move_to("paid")
        customer.record_purchase(order.quote.total)
        self.outbox.send(order_confirmation(customer, order))
        return order

    def ship(self, order_id: str) -> str:
        order = self.orders.ship(order_id)
        shipment = self.shipping.create(order_id)
        shipment.record("picked up by carrier")
        self.outbox.send(shipped_notice(self.customers.get(order.customer_id), order, shipment.tracking))
        return shipment.tracking

    def deliver(self, order_id: str) -> None:
        self.orders.get(order_id).move_to("delivered")
        self.shipping.for_order(order_id).record("delivered")

    def cancel(self, order_id: str) -> None:
        """Before it ships: stock goes back, and so does the money if it was paid."""
        order = self.orders.get(order_id)
        was_paid = order.status == "paid"
        self.orders.cancel(order_id)
        if was_paid:
            amount = self.payments.refundable(order_id)
            self.payments.refund(order_id, amount)
            self.outbox.send(refund_notice(self.customers.get(order.customer_id), order, amount))

    def return_items(self, order_id: str, lines: dict, today: date) -> ReturnReceipt:
        order = self.orders.get(order_id)
        receipt = process_return(order, lines, today, self.catalog, self.inventory, self.payments)
        if receipt.refunded.cents > 0:
            self.outbox.send(refund_notice(self.customers.get(order.customer_id), order, receipt.refunded))
        return receipt
