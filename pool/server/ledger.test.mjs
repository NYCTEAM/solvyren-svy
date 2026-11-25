import assert from 'node:assert/strict';
import {test} from 'node:test';
import {splitReward} from './ledger.mjs';
test('operator chooses fee; base atoms conserved, no network surcharge',()=>{
  for(const bps of [0,25,50,100,10000]){
    const rows=splitReward('100000000000001',[{wallet:'a',work:1},{wallet:'b',work:2}],bps);
    let sum=0n;
    for(const r of rows){
      assert.equal(BigInt(r.fee),BigInt(r.gross)*BigInt(bps)/10000n);
      sum+=BigInt(r.net)+BigInt(r.fee);
    }
    assert.equal(sum,100000000000001n);
  }
  assert.throws(()=>splitReward('123',[],25));
  assert.throws(()=>splitReward('1',[{wallet:'a',work:1}],10001));
});
