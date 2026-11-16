"""Money as whole cents, so totals never pick up rounding errors."""
from dataclasses import dataclass


@dataclass(frozen=True, order=True)
class Money:
    cents: int = 0

    def __post_init__(self):
        if not isinstance(self.cents, int):
            raise TypeError("Money is whole cents")

    def __add__(self, other: "Money") -> "Money":
        return Money(self.cents + other.cents)

    def __sub__(self, other: "Money") -> "Money":
        return Money(self.cents - other.cents)

    def times(self, quantity: int) -> "Money":
        return Money(self.cents * quantity)

    def percent(self, basis_points: int) -> "Money":
        """A share of this amount; 825 basis points is 8.25%. Rounds half up."""
        return Money((self.cents * basis_points + 5_000) // 10_000)

    def is_negative(self) -> bool:
        return self.cents < 0


ZERO = Money(0)


def total(amounts) -> Money:
    result = ZERO
    for amount in amounts:
        result = result + amount
    return result


def format_money(amount: Money) -> str:
    sign = "-" if amount.cents < 0 else ""
    dollars, cents = divmod(abs(amount.cents), 100)
    return f"{sign}${dollars:,}.{cents:02d}"
