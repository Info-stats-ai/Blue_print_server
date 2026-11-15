"""A month's invoices summed up."""
from ledger.money import Money, format_money


def monthly_total(invoices, rate_bp: int) -> Money:
    total = Money(0)
    for invoice in invoices:
        total = total + invoice.total(rate_bp)
    return total


def summary_line(invoices, rate_bp: int) -> str:
    return f"{len(invoices)} invoices, {format_money(monthly_total(invoices, rate_bp))}"
