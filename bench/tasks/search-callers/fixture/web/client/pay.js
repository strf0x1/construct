export function pay(order, amount) {
  return legacy_charge(order, amount);
}
