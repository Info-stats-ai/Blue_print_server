"""What delivery costs, and where a parcel is."""
from dataclasses import dataclass, field

from shop.money import Money, ZERO

FREE_SHIPPING_OVER = Money(7_500)
ZONE_BASE = {"domestic": Money(499), "international": Money(1_899)}
PER_EXTRA_KG = {"domestic": Money(150), "international": Money(600)}


class UnknownZone(ValueError):
    pass


def shipping_cost(weight_grams: int, zone: str, order_subtotal: Money) -> Money:
    """Base rate for the first kilo, then per started extra kilo. Free at home
    over the free-shipping line."""
    if zone not in ZONE_BASE:
        raise UnknownZone(zone)
    if zone == "domestic" and order_subtotal >= FREE_SHIPPING_OVER:
        return ZERO
    extra_kilos = max(0, (weight_grams - 1) // 1_000)
    return ZONE_BASE[zone] + PER_EXTRA_KG[zone].times(extra_kilos)


@dataclass
class Shipment:
    order_id: str
    tracking: str
    events: list = field(default_factory=list)

    def record(self, event: str) -> None:
        self.events.append(event)

    def status(self) -> str:
        return self.events[-1] if self.events else "label created"


class ShippingDesk:
    def __init__(self):
        self._shipments = {}
        self._next = 1

    def create(self, order_id: str) -> Shipment:
        shipment = Shipment(order_id, f"TRK{self._next:06d}")
        self._next += 1
        self._shipments[order_id] = shipment
        return shipment

    def for_order(self, order_id: str):
        return self._shipments.get(order_id)
