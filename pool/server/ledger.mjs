import {allocateExact} from './allocator.mjs';

export function splitReward(atoms, shares, bps) {
  if (!Number.isInteger(bps) || bps < 0 || bps > 10000) throw Error('Invalid fee in basis points');
  if (!shares.length) throw Error('Owned block has no verified shares; inspect before settling');
  return allocateExact(atoms, shares.map(({wallet, work}) => ({id:wallet, userId:wallet, weight:work})))
    .map(({id, amount}) => {
      const fee = BigInt(amount) * BigInt(bps) / 10000n;
      return {wallet:id, gross:amount, fee:String(fee), net:String(BigInt(amount)-fee)};
    });
}

export function transaction(db, fn) {
  db.exec('BEGIN IMMEDIATE');
  try { const result=fn(); db.exec('COMMIT'); return result; }
  catch (error) { db.exec('ROLLBACK'); throw error; }
}
