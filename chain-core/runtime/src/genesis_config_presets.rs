// This file is part of Substrate.

// Copyright (C) Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: Apache-2.0

// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// 	http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#[cfg(not(feature = "gpu-pow-rewards-dev"))]
use crate::SudoConfig;
use crate::{AccountId, BalancesConfig, RuntimeGenesisConfig};
use alloc::{vec, vec::Vec};
use frame_support::build_struct_json_patch;
use serde_json::Value;
use sp_consensus_aura::sr25519::AuthorityId as AuraId;
use sp_consensus_grandpa::AuthorityId as GrandpaId;
use sp_genesis_builder::{self, PresetId};
#[cfg(feature = "development-presets")]
use sp_keyring::Sr25519Keyring;

// Returns the genesis config presets populated with given parameters.
#[cfg(feature = "development-presets")]
fn testnet_genesis(
    initial_authorities: Vec<(AuraId, GrandpaId)>,
    endowed_accounts: Vec<AccountId>,
    root: AccountId,
) -> Value {
    // Test-only funding: this public, deterministic account must never be
    // used as the team's production wallet or a production genesis.
    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    return build_struct_json_patch!(RuntimeGenesisConfig {
        balances: BalancesConfig {
            balances: vec![(AccountId::new([9; 32]), pallet_pow_rewards::block_height_schedule::TEAM_GENESIS)]
        },
        aura: pallet_aura::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| x.0.clone())
                .collect::<Vec<_>>(),
        },
        grandpa: pallet_grandpa::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| (x.1.clone(), 1))
                .collect::<Vec<_>>(),
        },
        pow_rewards: pallet_pow_rewards::GenesisConfig {
            genesis_issued: pallet_pow_rewards::block_height_schedule::TEAM_GENESIS,
            initial_timestamp_seconds: 0,
            _marker: Default::default(),
        },
    });
    #[cfg(all(
        feature = "gpu-pow-rewards-dev",
        not(feature = "gpu-pow-v3-block-rewards-dev")
    ))]
    return build_struct_json_patch!(RuntimeGenesisConfig {
        balances: BalancesConfig { balances: vec![] },
        aura: pallet_aura::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| (x.0.clone()))
                .collect::<Vec<_>>(),
        },
        grandpa: pallet_grandpa::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| (x.1.clone(), 1))
                .collect::<Vec<_>>(),
        },
        pow_rewards: pallet_pow_rewards::GenesisConfig {
            genesis_issued: 0,
            initial_timestamp_seconds: 1_793_491_200,
            _marker: Default::default(),
        },
    });

    #[cfg(not(feature = "gpu-pow-rewards-dev"))]
    build_struct_json_patch!(RuntimeGenesisConfig {
        balances: BalancesConfig {
            balances: endowed_accounts
                .iter()
                .cloned()
                .chain(core::iter::once(crate::reward_account()))
                .map(|k| (k, 1u128 << 60))
                .chain(
                    #[cfg(feature = "incinerator-rehearsal")]
                    core::iter::once((crate::incinerator_rehearsal::DEAD, crate::UNIT)),
                    #[cfg(not(feature = "incinerator-rehearsal"))]
                    core::iter::empty(),
                )
                .collect::<Vec<_>>(),
        },
        aura: pallet_aura::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| (x.0.clone()))
                .collect::<Vec<_>>(),
        },
        grandpa: pallet_grandpa::GenesisConfig {
            authorities: initial_authorities
                .iter()
                .map(|x| (x.1.clone(), 1))
                .collect::<Vec<_>>(),
        },
        sudo: SudoConfig { key: Some(root) },
    })
}

/// Return the development genesis config.
#[cfg(feature = "development-presets")]
pub fn development_config_genesis() -> Value {
    testnet_genesis(
        vec![(
            sp_keyring::Sr25519Keyring::Alice.public().into(),
            sp_keyring::Ed25519Keyring::Alice.public().into(),
        )],
        vec![
            Sr25519Keyring::Alice.to_account_id(),
            Sr25519Keyring::Bob.to_account_id(),
            Sr25519Keyring::AliceStash.to_account_id(),
            Sr25519Keyring::BobStash.to_account_id(),
        ],
        sp_keyring::Sr25519Keyring::Alice.to_account_id(),
    )
}

/// Return the local genesis config preset.
#[cfg(feature = "development-presets")]
pub fn local_config_genesis() -> Value {
    testnet_genesis(
        vec![
            (
                sp_keyring::Sr25519Keyring::Alice.public().into(),
                sp_keyring::Ed25519Keyring::Alice.public().into(),
            ),
            (
                sp_keyring::Sr25519Keyring::Bob.public().into(),
                sp_keyring::Ed25519Keyring::Bob.public().into(),
            ),
        ],
        Sr25519Keyring::iter()
            .filter(|v| v != &Sr25519Keyring::One && v != &Sr25519Keyring::Two)
            .map(|v| v.to_account_id())
            .collect::<Vec<_>>(),
        Sr25519Keyring::Alice.to_account_id(),
    )
}

/// Provides the JSON representation of predefined genesis config for given `id`.
#[cfg(all(not(feature = "svy-matrix-live"), feature = "development-presets"))]
pub fn get_preset(id: &PresetId) -> Option<Vec<u8>> {
    let patch = match id.as_ref() {
        sp_genesis_builder::DEV_RUNTIME_PRESET => development_config_genesis(),
        sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET => local_config_genesis(),
        _ => return None,
    };
    Some(
        serde_json::to_string(&patch)
            .expect("serialization to json is expected to work. qed.")
            .into_bytes(),
    )
}

/// List of supported presets.
#[cfg(all(not(feature = "svy-matrix-live"), feature = "development-presets"))]
pub fn preset_names() -> Vec<PresetId> {
    vec![
        PresetId::from(sp_genesis_builder::DEV_RUNTIME_PRESET),
        PresetId::from(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET),
    ]
}

#[cfg(any(feature = "svy-matrix-live", not(feature = "development-presets")))]
pub fn get_preset(_: &PresetId) -> Option<Vec<u8>> {
    None
}

#[cfg(any(feature = "svy-matrix-live", not(feature = "development-presets")))]
pub fn preset_names() -> Vec<PresetId> {
    Vec::new()
}
