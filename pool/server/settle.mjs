import {DatabaseSync} from 'node:sqlite';
import {readFileSync} from 'node:fs';
import {setTimeout as delay} from 'node:timers/promises';
import {ApiPromise, WsProvider, Keyring} from '@polkadot/api';
import {cryptoWaitReady, checkAddress} from '@polkadot/util-crypto';
import {transaction, splitReward} from './ledger.mjs';

const genesis='0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8';
const cfg=JSON.parse(readFileSync(process.argv[2], 'utf8'));
await cryptoWaitReady();
function wallet(address) {
  const raw=address.replace(/^SVY/, '');
  if(!checkAddress(raw,42)[0]) throw Error('Invalid SVY address');
  return raw;
}
const owner=wallet(cfg.rewardWallet), operator=wallet(cfg.feeWallet);
if(!Number.isSafeInteger(cfg.startHeight)||cfg.startHeight<1||!Number.isInteger(cfg.feeBps)||cfg.feeBps<0||cfg.feeBps>10000)
  throw Error('Invalid start height or feeBps');
const db=new DatabaseSync(cfg.ledger);
const shares=new DatabaseSync(cfg.shares,{readOnly:true});
shares.exec('PRAGMA busy_timeout=5000');
db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
CREATE TABLE IF NOT EXISTS policy(id INTEGER PRIMARY KEY CHECK(id=1),value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS blocks(height INTEGER PRIMARY KEY,hash TEXT UNIQUE,beneficiary TEXT,reward TEXT);
CREATE TABLE IF NOT EXISTS splits(height INTEGER,wallet TEXT,gross TEXT,net TEXT,fee TEXT,PRIMARY KEY(height,wallet));
CREATE TABLE IF NOT EXISTS balances(wallet TEXT PRIMARY KEY,atoms TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS payments(hash TEXT PRIMARY KEY,wallet TEXT NOT NULL,atoms TEXT NOT NULL,signed TEXT NOT NULL,height INTEGER);
CREATE UNIQUE INDEX IF NOT EXISTS one_pending ON payments((1)) WHERE height IS NULL;`);
db.exec('CREATE INDEX IF NOT EXISTS paid_wallet_height ON payments(wallet,height)');
const policy=JSON.stringify([genesis,owner,operator,cfg.startHeight,cfg.feeBps]);
db.prepare('INSERT OR IGNORE INTO policy VALUES(1,?)').run(policy);
if(db.prepare('SELECT value FROM policy').get().value!==policy)throw Error('Existing ledger policy differs');
const api=await ApiPromise.create({provider:new WsProvider(cfg.archiveRpc),noInitWarn:true});
if(api.genesisHash.toHex()!==genesis)throw Error('Unexpected genesis');
const credentials=process.env.CREDENTIALS_DIRECTORY;
const signer=credentials ? new Keyring({type:'sr25519',ss58Format:42})
  .addFromUri(readFileSync(credentials+'/payout.seed','utf8').trim()) : null;
if(signer && signer.address!==owner)throw Error('Signing wallet differs from reward wallet');
function updateBalance(address,delta){
  const prior=db.prepare('SELECT atoms FROM balances WHERE wallet=?').get(address);
  const next=BigInt(prior?.atoms??0)+delta;
  if(next<0n)throw Error('Negative pool liability');
  db.prepare('INSERT OR REPLACE INTO balances VALUES(?,?)').run(address,String(next));
}
async function settle(height, prior){
  const hash=await api.rpc.chain.getBlockHash(height);
  const header=await api.rpc.chain.getHeader(hash);
  if(prior && header.parentHash.toHex()!==prior.hash)throw Error('Finalized checkpoint conflict');
  const at=await api.at(hash);
  const events=await at.query.system.events();
  const issued=events.filter(r=>r.event.section==='powRewards' && r.event.method==='Issued');
  if(issued.length!==1)throw Error('Unexpected reward event count');
  const [number,recipient,base]=issued[0].event.data;
  if(number.toNumber()!==height)throw Error('Unexpected reward height');
  const destination=wallet(recipient.toString());
  if(destination===owner && shares.prepare('SELECT height FROM watermark WHERE id=1').get().height<height){
    await delay(1000);
    return;
  }
  const rows=destination===owner ? splitReward(base.toString(),shares.prepare(
    'SELECT wallet,SUM(work) work FROM gpu_stream_shares WHERE height=? AND parent_hash=? GROUP BY wallet'
  ).all(height,header.parentHash.toHex()),cfg.feeBps) : [];
  const pending=db.prepare('SELECT * FROM payments WHERE height IS NULL').get();
  let paid=false;
  if(pending){
    const block=await api.rpc.chain.getBlock(hash);
    const index=block.block.extrinsics.findIndex(tx=>tx.hash.toHex()===pending.hash);
    if(index>=0){
      const group=events.filter(r=>r.phase.isApplyExtrinsic && r.phase.asApplyExtrinsic.toNumber()===index).map(r=>r.event);
      const transfers=group.filter(e=>e.section==='balances'&&e.method==='Transfer');
      if(!group.some(e=>e.section==='system'&&e.method==='ExtrinsicSuccess')||transfers.length!==1)
        throw Error('Finalized transfer failed; operator review required');
      const [from,to,value]=transfers[0].data;
      if(wallet(from.toString())!==owner||wallet(to.toString())!==pending.wallet||value.toString()!==pending.atoms)
        throw Error('Finalized transfer differs from intent');
      paid=true;
    }
  }
  transaction(db,()=>{
    db.prepare('INSERT INTO blocks VALUES(?,?,?,?)').run(height,hash.toHex(),destination,base.toString());
    for(const row of rows){
      const address=wallet(row.wallet);
      db.prepare('INSERT INTO splits VALUES(?,?,?,?,?)').run(height,address,row.gross,row.net,row.fee);
      updateBalance(address,BigInt(row.net));
      updateBalance(operator,BigInt(row.fee));
    }
    if(paid){
      db.prepare('UPDATE payments SET height=? WHERE hash=?').run(height,pending.hash);
      updateBalance(pending.wallet,-BigInt(pending.atoms));
    }
  });
}
async function payout(){
  if(!signer)return;
  let pending=db.prepare('SELECT * FROM payments WHERE height IS NULL').get();
  if(!pending){
    const due=db.prepare(`SELECT b.* FROM balances b WHERE b.wallet<>?
      ORDER BY (SELECT MAX(p.height) FROM payments p WHERE p.wallet=b.wallet),b.wallet`).all(owner)
      .find(row=>BigInt(row.atoms)>=1000000000000n);
    if(!due)return;
    const tx=api.tx.balances.transferKeepAlive(due.wallet,due.atoms);
    const head=await api.at(await api.rpc.chain.getFinalizedHead());
    const {data}=await head.query.system.account(owner);
    const {partialFee}=await tx.paymentInfo(signer);
    if(data.free.toBigInt()-data.frozen.toBigInt()<BigInt(due.atoms)+partialFee.toBigInt()+api.consts.balances.existentialDeposit.toBigInt())return;
    await tx.signAsync(signer,{nonce:await api.rpc.system.accountNextIndex(owner),era:0,tip:0});
    pending={hash:tx.hash.toHex(),signed:tx.toHex()};
    db.prepare('INSERT INTO payments VALUES(?,?,?,?,NULL)').run(pending.hash,due.wallet,due.atoms,pending.signed);
  }
  try {const hash=await api.rpc.author.submitExtrinsic(pending.signed);if(hash.toHex()!==pending.hash)throw Error('Submission hash mismatch');}
  catch(e){if(!/already imported|temporarily banned|stale|priority is too low/i.test(e.message))throw e;}
}
for(;;){
  const final=(await api.rpc.chain.getHeader(await api.rpc.chain.getFinalizedHead())).number.toNumber();
  const prior=db.prepare('SELECT height,hash FROM blocks ORDER BY height DESC LIMIT 1').get();
  const next=prior?prior.height+1:cfg.startHeight;
  if(next<=final){await settle(next,prior);continue;}
  if(prior && prior.height===final)await payout();
  await delay(3000);
}
