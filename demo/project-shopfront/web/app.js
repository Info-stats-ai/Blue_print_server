// The storefront page: one product and a cart counter.
let items = 0;

document.getElementById("add-to-cart").addEventListener("click", () => {
  items = 1;
  document.getElementById("cart").textContent = `Cart: ${items} item${items === 1 ? "" : "s"}`;
});
