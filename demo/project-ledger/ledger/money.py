"""Money in whole cents, never floats."""


class Money:
    def __init__(self, cents: int):
        if not isinstance(cents, int):
            raise TypeError("Money takes whole cents")
        self.cents = cents

    def __add__(self, other: "Money") -> "Money":
        return Money(self.cents + other.cents)

    def __sub__(self, other: "Money") -> "Money":
        return Money(self.cents - other.cents)

    def times(self, quantity: int) -> "Money":
        return Money(self.cents * quantity)

    def __eq__(self, other: object) -> bool:
        return isinstance(other, Money) and other.cents == self.cents

    def __repr__(self) -> str:
        return f"Money({self.cents})"


def format_money(amount: Money) -> str:
    sign = "-" if amount.cents < 0 else ""
    cents = abs(amount.cents)
    return f"{sign}${cents // 100}.{cents % 100:02d}"
