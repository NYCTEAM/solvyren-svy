//! Offline hash of a bounded, child-trie-free genesis storage list. No database/network.
use sp_runtime::{
    generic::Header,
    traits::{BlakeTwo256, Hash, Header as HeaderT},
    StateVersion,
};
use std::{collections::BTreeMap, fs::File, io::Read};
fn u32_at(bytes: &[u8], position: &mut usize) -> usize {
    let end = position.checked_add(4).expect("offset");
    let value = u32::from_le_bytes(
        bytes
            .get(*position..end)
            .expect("truncated length")
            .try_into()
            .unwrap(),
    );
    *position = end;
    value as usize
}
fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("bounded storage-list file required");
    let mut bytes = Vec::new();
    File::open(path)
        .unwrap()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    let mut position = 0;
    let count = u32_at(&bytes, &mut position);
    assert!(count > 0 && count <= 4096);
    let mut top = BTreeMap::new();
    for _ in 0..count {
        let key_len = u32_at(&bytes, &mut position);
        let value_len = u32_at(&bytes, &mut position);
        assert!(key_len > 0 && key_len <= 1024 && value_len <= 16 * 1024 * 1024);
        let key_end = position.checked_add(key_len).unwrap();
        let key = bytes
            .get(position..key_end)
            .expect("truncated key")
            .to_vec();
        position = key_end;
        let end = position.checked_add(value_len).unwrap();
        let value = bytes.get(position..end).expect("truncated value").to_vec();
        position = end;
        assert!(top.insert(key, value).is_none(), "duplicate storage key");
    }
    assert_eq!(position, bytes.len(), "trailing storage bytes");
    assert!(top.contains_key(b":code".as_slice()));
    for version in [StateVersion::V0, StateVersion::V1] {
        let root = BlakeTwo256::trie_root(top.clone().into_iter().collect(), version);
        let extrinsics = BlakeTwo256::ordered_trie_root(Vec::new(), version);
        let header = Header::<u32, BlakeTwo256>::new(
            0,
            extrinsics,
            root,
            Default::default(),
            Default::default(),
        );
        println!(
            "state_version={version:?} state_root={root:#x} genesis_hash={:#x}",
            header.hash()
        );
    }
}
