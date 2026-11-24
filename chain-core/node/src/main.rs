//! Substrate Node Template CLI library.
#![warn(missing_docs)]

#[cfg(not(windows))]
mod benchmarking;
mod chain_spec;
mod cli;
mod command;
mod eagw_work_provider;
pub mod gpu_pow;
mod gpu_pow_mx10;
mod smw_activation;
mod mx10_context;
mod gpu_pow_mx10_gpu;
pub mod gpu_pow_consensus;
mod adaptive_difficulty;
#[cfg(feature = "gpu-pow-v3-rewards-dev")]
mod gpu_pow_v3;
#[cfg(feature = "gpu-pow-v3-rewards-dev")]
mod gpu_pow_v3_consensus;
#[cfg(feature = "gpu-pow-v3-rewards-dev")]
mod gpu_pow_v3_mining;
#[cfg(any(not(feature = "gpu-pow-v3-rewards-dev"), feature = "gpu-pow-matrix-block-rewards-dev"))]
use gpu_pow_consensus as active_pow_consensus;
#[cfg(all(feature = "gpu-pow-v2", any(not(feature = "gpu-pow-v3-rewards-dev"), feature = "gpu-pow-matrix-block-rewards-dev")))]
use gpu_pow_mining as active_pow_mining;
#[cfg(all(feature = "gpu-pow-v3-rewards-dev", not(feature = "gpu-pow-matrix-block-rewards-dev")))]
use gpu_pow_v3_consensus as active_pow_consensus;
#[cfg(all(feature = "gpu-pow-v3-rewards-dev", not(feature = "gpu-pow-matrix-block-rewards-dev")))]
use gpu_pow_v3_mining as active_pow_mining;
#[cfg(feature = "gpu-pow-v2")]
mod gpu_pow_mining;
#[cfg(feature = "gpu-pow-v2")]
mod gpu_pow_select_chain;
#[cfg(test)]
mod gpu_queue_negative_test;
mod gpu_work_authoring;
pub mod gpu_work_binding;
mod gpu_work_import;
#[cfg(all(test, feature = "gpu-pow-rewards-dev"))]
mod reward_import_fixture_test;
#[cfg(all(test, feature = "gpu-pow-matrix-block-rewards-dev"))]
mod phase7_release_import_bench;
#[cfg(all(test, feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "svy-matrix-live")))]
mod t1_binding_test;
mod rpc;
mod service;
#[cfg(feature = "svy-matrix-live")]
mod svy_address;

fn main() -> sc_cli::Result<()> {
    command::run()
}
