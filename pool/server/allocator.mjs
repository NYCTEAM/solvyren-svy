function positiveInteger(value, name) {
  if (typeof value === 'bigint' && value > 0n) return value;
  if (!Number.isSafeInteger(value) || value <= 0) throw new Error(`${name} must be a positive safe integer`);
  return value;
}

export function atoms(value, name = 'atoms') {
  if (typeof value === 'bigint' && value >= 0n) return value;
  if (typeof value === 'string' && /^(0|[1-9]\d*)$/.test(value)) return BigInt(value);
  throw new Error(`${name} must be an unsigned decimal integer`);
}

export function allocateExact(totalValue, orders) {
  const total = atoms(totalValue, 'total');
  if (!Array.isArray(orders) || orders.length === 0) throw new Error('at least one active order is required');
  const normalized = orders.map((order) => ({
    id: String(order.id),
    userId: String(order.userId),
    weight: positiveInteger(order.weight, `weight for ${order.id}`),
  }));
  if (new Set(normalized.map((order) => order.id)).size !== normalized.length) throw new Error('duplicate order id');
  const totalWeight = normalized.reduce((sum, order) => sum + BigInt(order.weight), 0n);
  const rows = normalized.map((order) => {
    const product = total * BigInt(order.weight);
    return {...order, amount: product / totalWeight, remainder: product % totalWeight};
  });
  let undistributed = total - rows.reduce((sum, row) => sum + row.amount, 0n);
  const priority = [...rows].sort((a, b) => {
    if (a.remainder !== b.remainder) return a.remainder > b.remainder ? -1 : 1;
    return a.id.localeCompare(b.id);
  });
  for (let index = 0; undistributed > 0n; index++, undistributed--) priority[index].amount++;
  return rows.map(({remainder, ...row}) => ({...row, weight: typeof row.weight === 'bigint' ? (row.weight <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(row.weight) : row.weight.toString()) : row.weight, amount: row.amount.toString(), totalWeight: totalWeight.toString()}));
}

export function formatToken(value, decimals = 12) {
  const amount = atoms(value);
  const scale = 10n ** BigInt(decimals);
  const whole = amount / scale;
  const fraction = (amount % scale).toString().padStart(decimals, '0').replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole.toString();
}
