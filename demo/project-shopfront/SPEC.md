# Shopfront: what it must do

1. **Prices.** Subtotal, less one discount (the better of a coupon and the
   customer's loyalty discount; they don't stack), plus 8.25% tax on what's
   left, plus shipping.
2. **Shipping.** A base rate covers the first kilo; each started extra kilo
   costs more. Domestic orders of $75.00 or more ship free. International
   orders never do.
3. **Stock.** Placing an order holds its stock. All lines are held or none.
   Cancelling gives it back. Shipping takes it off the shelf.
4. **Payments.** A declined card cancels the order and gives the stock back.
   Refunds go to the card that paid and never exceed what was paid.
5. **Returns.** Within 30 days of a delivered order. The refund is the
   returned items' share of what was paid for goods (discount and tax in
   proportion); shipping isn't refunded.
6. **Messages.** Customers hear when an order is confirmed, shipped or
   refunded.
7. **Reports.** Sales by day, top products, average order and refund rate
   count paid, shipped and delivered orders only.
