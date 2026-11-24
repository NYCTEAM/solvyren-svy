//! Chain-bound per-block ASERT upgrade for the existing public development chain.
//! Only validated parent history is used; hardware telemetry is never an input.
use crate::gpu_pow_consensus::DifficultyChain;
use eagle_runtime::opaque::Block;
use sc_consensus_pow::Error;
use sp_core::{H256, U256, U512};
use sp_runtime::traits::Header as _;
use std::{collections::{HashMap, VecDeque}, sync::{Mutex, OnceLock}};

pub(crate) const ACTIVATION_HEIGHT: u32 = 17_517;
const SPACING_MS: u64 = 10_000;
const HALF_LIFE_MS: i128 = 300_000;
const BOOTSTRAP_INTERVALS: u32 = 12;
const MAX_DIFFICULTY: u128 = 1u128 << 120;
#[derive(Clone, Copy)]
struct Anchor { height: u32, timestamp: u64, difficulty: u128 }
#[derive(Default)]
struct Cache { values: HashMap<H256, Anchor>, order: VecDeque<H256> }
static ANCHORS: OnceLock<Mutex<Cache>> = OnceLock::new();
fn failure(message: &str) -> Error<Block> { Error::Other(message.into()) }

/// Full-width integer ASERT polynomial, rounded to the node's U128 work units.
fn adjusted(reference: u128, drift_ms: i128) -> Result<u128, Error<Block>> {
    if reference == 0 || reference > MAX_DIFFICULTY { return Err(failure("invalid ASERT reference difficulty")); }
    let exponent = drift_ms.checked_mul(65536).ok_or_else(|| failure("ASERT exponent overflow"))? / HALF_LIFE_MS;
    let shifts = exponent >> 16;
    let fraction = (exponent - (shifts << 16)) as u128;
    let factor = 65536 + ((195766423245049u128 * fraction + 971821376u128 * fraction.pow(2) + 5127u128 * fraction.pow(3) + (1u128 << 47)) >> 48);
    if shifts > 255 { return Ok(1); }
    if shifts < -255 { return Ok(MAX_DIFFICULTY); }
    let reference_target = U256::MAX / U256::from(reference);
    let mut target = U512::from(reference_target) * U512::from(factor);
    if shifts >= 0 {
        if target.bits() as i128 + shifts > 512 { return Ok(1); }
        target <<= shifts as usize;
    } else { target >>= (-shifts) as usize; }
    target >>= 16;
    target = target.max(U512::from(U256::MAX / U256::from(MAX_DIFFICULTY))).min(U512::from(U256::MAX));
    let bytes = target.to_big_endian();
    let bounded = U256::from_big_endian(&bytes[32..]);
    Ok((U256::MAX / bounded).max(U256::one()).min(U256::from(MAX_DIFFICULTY)).as_u128())
}

fn bootstrap<C: DifficultyChain>(client: &C, hash: H256) -> Result<Anchor, Error<Block>> {
    let header = client.pow_header(hash)?;
    if *header.number() != ACTIVATION_HEIGHT - 1 { return Err(failure("wrong ASERT anchor height")); }
    if let Some((timestamp,difficulty)) = client.pow_asert_anchor(hash)? {
        if timestamp == 0 || difficulty == 0 || difficulty > MAX_DIFFICULTY { return Err(failure("invalid persisted ASERT anchor values")); }
        return Ok(Anchor { height: ACTIVATION_HEIGHT-1,timestamp,difficulty });
    }
    let timestamp = client.pow_timestamp(hash)?;
    let mut later = timestamp;
    let mut cursor = hash;
    let mut work = U256::zero();
    let old = client.pow_aux(hash)?.ok_or_else(|| failure("missing ASERT anchor work"))?.difficulty;
    if old == 0 || old > MAX_DIFFICULTY { return Err(failure("invalid ASERT anchor work")); }
    for offset in 0..BOOTSTRAP_INTERVALS {
        let current = client.pow_header(cursor)?;
        if current.hash()!=cursor || *current.number()!=ACTIVATION_HEIGHT-1-offset { return Err(failure("invalid ASERT bootstrap ancestry")); }
        let aux = client.pow_aux(cursor)?.ok_or_else(|| failure("missing ASERT bootstrap work"))?;
        if aux.difficulty == 0 || aux.difficulty > MAX_DIFFICULTY { return Err(failure("invalid ASERT bootstrap work")); }
        work += U256::from(aux.difficulty);
        let parent = *current.parent_hash();
        let earlier = client.pow_timestamp(parent)?;
        if earlier >= later { return Err(failure("non-increasing ASERT bootstrap timestamp")); }
        later = earlier; cursor = parent;
    }
    let elapsed = timestamp - later;
    let estimate = work * U256::from(SPACING_MS) / U256::from(elapsed);
    let lower = U256::from((old / 64).max(1));
    let upper = (U256::from(old) * U256::from(64)).min(U256::from(MAX_DIFFICULTY));
    let difficulty=estimate.max(lower).min(upper).max(U256::one()).as_u128();
    client.pow_store_asert_anchor(hash,(timestamp,difficulty))?;
    Ok(Anchor { height: ACTIVATION_HEIGHT - 1, timestamp, difficulty })
}

fn anchor<C: DifficultyChain>(client: &C, parent: H256) -> Result<Anchor, Error<Block>> {
    let cache = ANCHORS.get_or_init(|| Mutex::new(Cache::default()));
    let mut cursor = parent;
    let mut visited = Vec::new();
    let result = loop {
        if let Some(found) = cache.lock().map_err(|_| failure("ASERT cache lock"))?.values.get(&cursor).copied() { break found; }
        let header = client.pow_header(cursor)?;
        if header.hash()!=cursor { return Err(failure("ASERT header hash mismatch")); }
        if *header.number() == ACTIVATION_HEIGHT {
            // Start the schedule at the first upgraded block. A maintenance
            // pause before activation must not cause a compensating block burst.
            let (timestamp,difficulty)=if let Some(value)=client.pow_asert_anchor(cursor)? {value} else {
                let value=(client.pow_timestamp(cursor)?,client.pow_aux(cursor)?.ok_or_else(||failure("missing first ASERT block work"))?.difficulty);
                client.pow_store_asert_anchor(cursor,value)?;value
            };
            if timestamp==0 || difficulty==0 || difficulty>MAX_DIFFICULTY {return Err(failure("invalid first ASERT block anchor"));}
            break Anchor {height:ACTIVATION_HEIGHT,timestamp,difficulty};
        }
        if *header.number() < ACTIVATION_HEIGHT { return Err(failure("ASERT parent precedes activation")); }
        // Cache recent descendants, never an unbounded history path.
        if visited.len() < 128 { visited.push(cursor); }
        cursor = *header.parent_hash();
    };
    let mut locked = cache.lock().map_err(|_| failure("ASERT cache lock"))?;
    for hash in visited.into_iter().rev().chain(std::iter::once(parent)) {
        if locked.values.contains_key(&hash) { continue; }
        while locked.order.len() >= 4096 { if let Some(old) = locked.order.pop_front() { locked.values.remove(&old); } }
        locked.values.insert(hash, result); locked.order.push_back(hash);
    }
    Ok(result)
}

pub(crate) fn difficulty<C: DifficultyChain>(client: &C, parent: H256) -> Result<u128, Error<Block>> {
    let header = client.pow_header(parent)?;
    if *header.number()==ACTIVATION_HEIGHT-1 {return Ok(bootstrap(client,parent)?.difficulty);}
    let reference = anchor(client, parent)?;
    let timestamp = client.pow_timestamp(parent)?;
    if timestamp < reference.timestamp { return Err(failure("ASERT parent time precedes anchor")); }
    let blocks = header.number().checked_sub(reference.height).ok_or_else(|| failure("ASERT height underflow"))?;
    let drift = i128::from(timestamp - reference.timestamp) - i128::from(blocks) * i128::from(SPACING_MS);
    adjusted(reference.difficulty, drift)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eagle_runtime::Header;
    use sp_runtime::generic::Digest;
    use sc_consensus_pow::PowAux;
    struct Chain { headers: HashMap<H256, Header>, times: HashMap<H256,u64>, aux: HashMap<H256,PowAux<u128>>, anchors: Mutex<HashMap<H256,(u64,u128)>> }
    impl DifficultyChain for Chain {
        fn pow_header(&self,h:H256)->Result<Header,Error<Block>> { self.headers.get(&h).cloned().ok_or_else(||failure("missing fixture header")) }
        fn pow_timestamp(&self,h:H256)->Result<u64,Error<Block>> { self.times.get(&h).copied().ok_or_else(||failure("missing fixture timestamp")) }
        fn pow_aux(&self,h:H256)->Result<Option<PowAux<u128>>,Error<Block>> { Ok(self.aux.get(&h).cloned()) }
        fn pow_asert_anchor(&self,h:H256)->Result<Option<(u64,u128)>,Error<Block>> { Ok(self.anchors.lock().unwrap().get(&h).copied()) }
        fn pow_store_asert_anchor(&self,h:H256,v:(u64,u128))->Result<(),Error<Block>> {self.anchors.lock().unwrap().insert(h,v);Ok(())}
    }
    fn append(c:&mut Chain, number:u32,parent:H256,time:u64,diff:u128)->H256 {
        let h=Header::new(number,H256::from_low_u64_be(time),H256::from_low_u64_be(diff as u64),parent,Digest::default());
        let hash=h.hash();c.headers.insert(hash,h);c.times.insert(hash,time);c.aux.insert(hash,PowAux{difficulty:diff,total_difficulty:diff*100000});hash
    }
    fn fixture(interval:u64,diff:u128)->(Chain,H256) {
        let mut c=Chain{headers:HashMap::new(),times:HashMap::new(),aux:HashMap::new(),anchors:Mutex::default()};
        let mut parent=H256::repeat_byte(91);c.times.insert(parent,1_000_000);
        for i in 0..BOOTSTRAP_INTERVALS { parent=append(&mut c,ACTIVATION_HEIGHT-BOOTSTRAP_INTERVALS+i,parent,1_000_000+u64::from(i+1)*interval,diff); }
        (c,parent)
    }
    #[test] fn asert_integer_math_bounds_and_signed_rounding() {
        assert_eq!(adjusted(200000,0).unwrap(),200000);
        assert!((adjusted(200000,300000).unwrap() as i64-100000).abs()<=1);
        assert!((adjusted(200000,-300000).unwrap() as i64-400000).abs()<=1);
        assert_eq!(adjusted(200000,i128::from(u64::MAX)).unwrap(),1);
        assert_eq!(adjusted(200000,-i128::from(u64::MAX)).unwrap(),MAX_DIFFICULTY);
        assert!(adjusted(0,0).is_err());
    }
    #[test] fn bootstrap_matches_completed_cpu_work_and_is_replayable() {
        let (c,parent)=fixture(350000,7000000);
        assert_eq!(difficulty(&c,parent).unwrap(),200000);
        assert_eq!(difficulty(&c,parent).unwrap(),200000);
    }
    #[test] fn branches_use_their_own_anchor_and_completed_time() {
        let (mut c,parent)=fixture(10000,200000);
        let time=c.times[&parent]+10000;
        let first=append(&mut c,ACTIVATION_HEIGHT,parent,time,200000);
        let fast=append(&mut c,ACTIVATION_HEIGHT+1,first,time+1000,200000);
        let slow=append(&mut c,ACTIVATION_HEIGHT+1,first,time+310000,200000);
        assert!(difficulty(&c,fast).unwrap()>200000);
        assert_eq!(difficulty(&c,slow).unwrap(),100000);
        let (other,other_parent)=fixture(20000,200000);
        assert_eq!(difficulty(&other,other_parent).unwrap(),100000);
        assert!(difficulty(&c,fast).unwrap()>200000);
    }
    #[test] fn invalid_bootstrap_is_rejected_before_caching() {
        let (mut c,parent)=fixture(10000,211111);
        let time=c.times[&parent];c.times.insert(*c.headers[&parent].parent_hash(),time);
        assert!(bootstrap(&c,parent).is_err());
    }
    #[test] fn persisted_anchor_survives_pruned_state_and_rejects_corruption() {
        let (mut c,parent)=fixture(10000,233333);
        assert_eq!(bootstrap(&c,parent).unwrap().difficulty,233333);
        c.times.clear();
        assert_eq!(bootstrap(&c,parent).unwrap().difficulty,233333);
        c.anchors.lock().unwrap().insert(parent,(0,233333));
        assert!(bootstrap(&c,parent).is_err());
    }
    #[test] fn activation_gate_is_chain_bound_and_shared_with_fork_choice() {
        use sc_consensus_pow::PowAlgorithm;
        use crate::gpu_pow_consensus::DynamicDifficultyAlgorithm;
        use std::sync::Arc;
        let (c,parent)=fixture(350000,7000000);
        let chain=Arc::new(c);
        let genesis=crate::mx10_context::PUBLICDEV_GENESIS.parse().unwrap();
        let default_fork_choice=DynamicDifficultyAlgorithm::new(genesis,chain.clone());
        assert_eq!(default_fork_choice.difficulty(parent).unwrap(),200000);
        let unrelated=DynamicDifficultyAlgorithm::new(H256::repeat_byte(8),chain);
        assert_eq!(unrelated.difficulty(parent).unwrap(),7000000);
    }
    #[test] fn maintenance_gap_does_not_trigger_a_block_burst() {
        let (mut c,parent)=fixture(10000,244444);
        let time=c.times[&parent]+1_800_000;
        let first=append(&mut c,ACTIVATION_HEIGHT,parent,time,244444);
        assert_eq!(difficulty(&c,first).unwrap(),244444);
        let cached=anchor(&c,first).unwrap();
        c.times.clear();
        assert_eq!(anchor(&c,first).unwrap().timestamp,cached.timestamp);
    }
}
