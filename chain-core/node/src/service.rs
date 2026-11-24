//! Service and ServiceFactory implementation. Specialized wrapper over substrate service.

use eagle_runtime::{self, apis::RuntimeApi, opaque::Block};
use futures::FutureExt;
use sc_client_api::{Backend, BlockBackend};
#[cfg(not(feature = "gpu-pow-v2"))]
use sc_consensus_aura::{BuildVerifierParams, SlotProportion, StartAuraParams};
use sc_consensus_grandpa::SharedVoterState;
use sc_service::{error::Error as ServiceError, Configuration, TaskManager, WarpSyncConfig};
use sc_telemetry::{Telemetry, TelemetryWorker};
use sc_transaction_pool_api::OffchainTransactionPoolFactory;
#[cfg(not(feature = "gpu-pow-v2"))]
use sp_consensus_aura::sr25519::AuthorityPair as AuraPair;
#[cfg(feature = "gpu-pow-v2")]
use std::time::Instant;
use std::{sync::Arc, time::Duration};

pub(crate) type FullClient = sc_service::TFullClient<
    Block,
    RuntimeApi,
    sc_executor::WasmExecutor<sp_io::SubstrateHostFunctions>,
>;
pub(crate) type FullBackend = sc_service::TFullBackend<Block>;
#[cfg(feature = "gpu-pow-v2")]
type FullSelectChain = crate::gpu_pow_select_chain::PowWorkSelectChain;
#[cfg(not(feature = "gpu-pow-v2"))]
type FullSelectChain = sc_consensus::LongestChain<FullBackend, Block>;

/// The minimum period of blocks on which justifications will be
/// imported and generated.
const GRANDPA_JUSTIFICATION_PERIOD: u32 = 512;

pub type Service = sc_service::PartialComponents<
    FullClient,
    FullBackend,
    FullSelectChain,
    sc_consensus::DefaultImportQueue<Block>,
    sc_transaction_pool::TransactionPoolHandle<Block, FullClient>,
    (
        sc_consensus_grandpa::GrandpaBlockImport<FullBackend, Block, FullClient, FullSelectChain>,
        sc_consensus_grandpa::LinkHalf<Block, FullClient, FullSelectChain>,
        Option<Telemetry>,
    ),
>;

fn validate_chain_mode(id: &str) -> Result<(), ServiceError> {
    if cfg!(feature = "svy-matrix-live") {
        return if id == crate::chain_spec::SVY_MATRIX_LIVE_ID {
            Ok(())
        } else {
            Err(ServiceError::Other("SVY Live build requires the pinned Live chain".into()))
        };
    }
    if id == crate::chain_spec::SVY_MATRIX_LIVE_ID {
        return Err(ServiceError::Other("SVY Live chain requires the dedicated Live build".into()));
    }
    if crate::chain_spec::is_mx10_isolated_id(id) {
        return if cfg!(feature = "gpu-pow-matrix-block-rewards-dev") { Ok(()) }
        else { Err(ServiceError::Other("MX10 isolated chain requires matrix test feature".into())) };
    }
    let is_pow_v2 = id == crate::chain_spec::POW_LOCAL_ID;
    let is_rewards = id == crate::chain_spec::POW_REWARDS_LOCAL_ID;
    let is_v3_rewards = id == crate::chain_spec::POW_V3_REWARDS_LOCAL_ID;
    let is_v3_block_rewards = id == crate::chain_spec::POW_V3_BLOCK_REWARDS_LOCAL_ID;
    let is_matrix_block_rewards = id == crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID;
    let expected = if cfg!(feature = "gpu-pow-matrix-block-rewards-dev") {
        Some(crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID)
    } else if cfg!(feature = "gpu-pow-v3-block-rewards-dev") {
        Some(crate::chain_spec::POW_V3_BLOCK_REWARDS_LOCAL_ID)
    } else if cfg!(feature = "gpu-pow-v3-rewards-dev") {
        Some(crate::chain_spec::POW_V3_REWARDS_LOCAL_ID)
    } else if cfg!(feature = "gpu-pow-rewards-dev") {
        Some(crate::chain_spec::POW_REWARDS_LOCAL_ID)
    } else if cfg!(feature = "gpu-pow-v2") {
        Some(crate::chain_spec::POW_LOCAL_ID)
    } else {
        None
    };
    if expected != Some(id)
        && (expected.is_some() || is_pow_v2 || is_rewards || is_v3_rewards || is_v3_block_rewards || is_matrix_block_rewards)
    {
        return Err(ServiceError::Other(
            "binary feature and isolated chain-spec identity do not match".into(),
        ));
    }
    Ok(())
}

#[cfg(feature = "gpu-pow-v2")]
fn pow_finality_only_from_environment() -> Result<bool, ServiceError> {
    match std::env::var_os("EAGW_POW_FINALITY_ONLY") {
        None => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(ServiceError::Other(
            "EAGW_POW_FINALITY_ONLY must be absent or exactly 1".into(),
        )),
    }
}

#[cfg(feature = "gpu-pow-v2")]
#[derive(Clone, Copy)]
struct PowAuthoringBudget {
    import_limit: u64,
    attempt_limit: u64,
    max_duration: Duration,
}

#[cfg(feature = "gpu-pow-v2")]
fn pow_authoring_budget_from_environment() -> Result<PowAuthoringBudget, ServiceError> {
    let value = std::env::var_os("EAGW_POW_AUTHORING_LIMIT").ok_or_else(|| {
        ServiceError::Other("GPU PoW author requires EAGW_POW_AUTHORING_LIMIT".into())
    })?;
    let value = value.to_str().ok_or_else(|| {
        ServiceError::Other("EAGW_POW_AUTHORING_LIMIT must be valid UTF-8".into())
    })?;
    let limit = value.parse::<u64>().map_err(|_| {
        ServiceError::Other("EAGW_POW_AUTHORING_LIMIT must be a positive integer".into())
    })?;
    let max_imports = if cfg!(all(feature = "gpu-pow-v3-rewards-dev", not(feature = "gpu-pow-matrix-block-rewards-dev"))) {
        256
    } else {
        64
    };
    let max_attempts = if cfg!(all(feature = "gpu-pow-v3-rewards-dev", not(feature = "gpu-pow-matrix-block-rewards-dev"))) {
        4096
    } else {
        64
    };
    if !(1..=max_imports).contains(&limit) {
        return Err(ServiceError::Other(format!(
            "EAGW_POW_AUTHORING_LIMIT must be 1..={max_imports}"
        )));
    }
    let attempts = std::env::var("EAGW_POW_SESSION_MAX_ATTEMPTS")
        .map_err(|_| {
            ServiceError::Other("GPU PoW author requires EAGW_POW_SESSION_MAX_ATTEMPTS".into())
        })?
        .parse::<u64>()
        .map_err(|_| {
            ServiceError::Other(format!(
                "EAGW_POW_SESSION_MAX_ATTEMPTS must be 1..={max_attempts}"
            ))
        })?;
    if !(limit..=max_attempts).contains(&attempts) {
        return Err(ServiceError::Other(format!(
            "EAGW_POW_SESSION_MAX_ATTEMPTS must be between the authoring limit and {max_attempts}"
        )));
    }
    let seconds = std::env::var("EAGW_POW_SESSION_MAX_SECONDS")
        .map_err(|_| {
            ServiceError::Other("GPU PoW author requires EAGW_POW_SESSION_MAX_SECONDS".into())
        })?
        .parse::<u64>()
        .map_err(|_| ServiceError::Other("EAGW_POW_SESSION_MAX_SECONDS must be 1..=3600".into()))?;
    if !(1..=3600).contains(&seconds) {
        return Err(ServiceError::Other(
            "EAGW_POW_SESSION_MAX_SECONDS must be 1..=3600".into(),
        ));
    }
    Ok(PowAuthoringBudget {
        import_limit: limit,
        attempt_limit: attempts,
        max_duration: Duration::from_secs(seconds),
    })
}

#[cfg(test)]
mod mode_tests {
    #[test]
    fn pow_spec_and_feature_must_match_before_client_creation() {
        let expected = if cfg!(feature = "svy-matrix-live") {
            crate::chain_spec::SVY_MATRIX_LIVE_ID
        } else if cfg!(feature = "gpu-pow-matrix-block-rewards-dev") {
            crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID
        } else if cfg!(feature = "gpu-pow-v3-block-rewards-dev") {
            crate::chain_spec::POW_V3_BLOCK_REWARDS_LOCAL_ID
        } else if cfg!(feature = "gpu-pow-v3-rewards-dev") {
            crate::chain_spec::POW_V3_REWARDS_LOCAL_ID
        } else if cfg!(feature = "gpu-pow-rewards-dev") {
            crate::chain_spec::POW_REWARDS_LOCAL_ID
        } else {
            crate::chain_spec::POW_LOCAL_ID
        };
        assert_eq!(
            super::validate_chain_mode(expected).is_ok(),
            cfg!(feature = "gpu-pow-v2")
        );
        for isolated_id in [
            crate::chain_spec::POW_LOCAL_ID,
            crate::chain_spec::POW_REWARDS_LOCAL_ID,
            crate::chain_spec::POW_V3_REWARDS_LOCAL_ID,
            crate::chain_spec::POW_V3_BLOCK_REWARDS_LOCAL_ID,
            crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID,
            crate::chain_spec::SVY_MATRIX_LIVE_ID,
        ] {
            assert_eq!(super::validate_chain_mode(isolated_id).is_ok(),
                cfg!(feature = "gpu-pow-v2") && isolated_id == expected);
        }
        assert_eq!(
            super::validate_chain_mode(crate::chain_spec::POW_MX10_PUBLICDEV_ID).is_ok(),
            cfg!(feature = "gpu-pow-matrix-block-rewards-dev")
        );
        if cfg!(feature = "gpu-pow-v2") {
            let wrong = if cfg!(feature = "gpu-pow-rewards-dev") {
                crate::chain_spec::POW_LOCAL_ID
            } else {
                crate::chain_spec::POW_REWARDS_LOCAL_ID
            };
            assert!(super::validate_chain_mode(wrong).is_err());
            if cfg!(feature = "gpu-pow-v3-rewards-dev") {
                assert!(
                    super::validate_chain_mode(crate::chain_spec::POW_REWARDS_LOCAL_ID).is_err()
                );
            } else {
                assert!(
                    super::validate_chain_mode(crate::chain_spec::POW_V3_REWARDS_LOCAL_ID).is_err()
                );
            }
        }
        assert_eq!(
            super::validate_chain_mode("local_testnet").is_ok(),
            !cfg!(feature = "gpu-pow-v2")
        );
        assert_eq!(
            super::validate_chain_mode("dev").is_ok(),
            !cfg!(feature = "gpu-pow-v2")
        );
    }
}

pub fn new_partial(config: &Configuration) -> Result<Service, ServiceError> {
    // Fail before telemetry, client creation, or opening any database.
    validate_chain_mode(config.chain_spec.id())?;
    if cfg!(feature = "svy-matrix-live") && config.dev_key_seed.is_some() {
        return Err(ServiceError::Other("Development keys are disabled for SVY Live".into()));
    }
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    let work_algorithm = crate::gpu_pow_consensus::WorkAlgorithm::from_spec(config.chain_spec.as_ref())
        .map_err(ServiceError::Other)?;
    let telemetry = config
        .telemetry_endpoints
        .clone()
        .filter(|x| !x.is_empty())
        .map(|endpoints| -> Result<_, sc_telemetry::Error> {
            let worker = TelemetryWorker::new(16)?;
            let telemetry = worker.handle().new_telemetry(endpoints);
            Ok((worker, telemetry))
        })
        .transpose()?;

    let executor = sc_service::new_wasm_executor::<sp_io::SubstrateHostFunctions>(&config.executor);
    let (client, backend, keystore_container, task_manager) =
        sc_service::new_full_parts::<Block, RuntimeApi, _>(
            config,
            telemetry.as_ref().map(|(_, telemetry)| telemetry.handle()),
            executor,
        )?;
    let client = Arc::new(client);

    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    if let crate::gpu_pow_consensus::WorkAlgorithm::Mx10N16(context) = work_algorithm {
        let actual = client.block_hash(0).map_err(|e| ServiceError::Other(format!("cannot read MX10 genesis: {e}")))?
            .ok_or_else(|| ServiceError::Other("missing MX10 genesis".into()))?;
        context.ensure_genesis(actual).map_err(ServiceError::Other)?;
    }

    let telemetry = telemetry.map(|(worker, telemetry)| {
        task_manager
            .spawn_handle()
            .spawn("telemetry", None, worker.run());
        telemetry
    });

    #[cfg(feature = "gpu-pow-v2")]
    let select_chain =
        crate::gpu_pow_select_chain::PowWorkSelectChain::new(backend.clone(), client.clone());
    #[cfg(not(feature = "gpu-pow-v2"))]
    let select_chain = sc_consensus::LongestChain::new(backend.clone());

    let transaction_pool = Arc::from(
        sc_transaction_pool::Builder::new(
            task_manager.spawn_essential_handle(),
            client.clone(),
            config.role.is_authority().into(),
        )
        .with_options(config.transaction_pool.clone())
        .with_prometheus(config.prometheus_registry())
        .build(),
    );

    let (grandpa_block_import, grandpa_link) = sc_consensus_grandpa::block_import(
        client.clone(),
        GRANDPA_JUSTIFICATION_PERIOD,
        &client,
        select_chain.clone(),
        telemetry.as_ref().map(|x| x.handle()),
    )?;

    #[cfg(not(feature = "gpu-pow-v2"))]
    let import_queue = {
        let cidp_client = client.clone();
        let aura_verifier =
            sc_consensus_aura::build_verifier::<AuraPair, _, _, Block>(BuildVerifierParams {
                client: client.clone(),
                create_inherent_data_providers: move |parent_hash, _| {
                    let cidp_client = cidp_client.clone();
                    async move {
                        let slot_duration = sc_consensus_aura::standalone::slot_duration_at(
                            &*cidp_client,
                            parent_hash,
                        )?;
                        let timestamp = sp_timestamp::InherentDataProvider::from_system_time();

                        let slot =
						sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_slot_duration(
							*timestamp,
							slot_duration,
						);

                        Ok((slot, timestamp))
                    }
                },
                check_for_equivocation: Default::default(),
                telemetry: telemetry.as_ref().map(|x| x.handle()),
                compatibility_mode: Default::default(),
            });
        #[cfg(not(feature = "incinerator-rehearsal"))]
        let verifier = crate::gpu_work_import::AuraThenWork {
            aura: aura_verifier,
            work: crate::gpu_work_import::DeterministicCpuWorkVerifier,
        };
        #[cfg(feature = "incinerator-rehearsal")]
        let verifier = aura_verifier;
        sc_consensus::import_queue::BasicQueue::new(
            verifier,
            Box::new(grandpa_block_import.clone()),
            Some(Box::new(grandpa_block_import.clone())),
            &task_manager.spawn_essential_handle(),
            config.prometheus_registry(),
        )
    };

    // Experimental fixed target only: not a dynamic difficulty, GPU proof,
    // production issuance or permission to reuse an existing Aura database.
    #[cfg(feature = "gpu-pow-v2")]
    let import_queue = {
        let genesis = client
            .block_hash(0)
            .map_err(|e| ServiceError::Other(format!("cannot read PoW genesis hash: {e}")))?
            .ok_or_else(|| ServiceError::Other("missing PoW genesis hash".into()))?;
        let algorithm =
            crate::active_pow_consensus::DynamicDifficultyAlgorithm::new(genesis, client.clone());
        #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
        let algorithm = algorithm.with_work_algorithm(work_algorithm);
        let inherent = move |_, ()| async move {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                sp_timestamp::InherentDataProvider::from_system_time(),
            )
        };
        let pow_import = sc_consensus_pow::PowBlockImport::new(
            grandpa_block_import.clone(),
            client.clone(),
            algorithm.clone(),
            0,
            select_chain.clone(),
            inherent,
        );
        let verifier = crate::active_pow_consensus::StrictPowVerifier::new(algorithm);
        sc_consensus::import_queue::BasicQueue::new(
            verifier,
            Box::new(pow_import),
            Some(Box::new(grandpa_block_import.clone())),
            &task_manager.spawn_essential_handle(),
            config.prometheus_registry(),
        )
    };

    Ok(sc_service::PartialComponents {
        client,
        backend,
        task_manager,
        import_queue,
        keystore_container,
        select_chain,
        transaction_pool,
        other: (grandpa_block_import, grandpa_link, telemetry),
    })
}

/// Builds a new service for a full client.
pub fn new_full<
    N: sc_network::NetworkBackend<Block, <Block as sp_runtime::traits::Block>::Hash>,
>(
    config: Configuration,
    gpu_pow_author: bool,
    gpu_pow_beneficiary: Option<String>,
) -> Result<TaskManager, ServiceError> {
    // Reject a binary/spec mismatch before reading GPU author environment or
    // reserving the host-wide GPU owner. `new_partial` repeats this check so
    // callers that only construct partial components keep the same boundary.
    validate_chain_mode(config.chain_spec.id())?;
    if crate::chain_spec::is_mx10_isolated_id(config.chain_spec.id()) && gpu_pow_author {
        return Err(ServiceError::Other("MX10 test chain has verification/import only; V2 GPU authoring is disabled".into()));
    }
    if gpu_pow_author != gpu_pow_beneficiary.is_some() {
        return Err(ServiceError::Other(
            "--gpu-pow-author and --gpu-pow-beneficiary must be supplied together".into(),
        ));
    }
    #[cfg(not(feature = "gpu-pow-v2"))]
    if gpu_pow_author {
        return Err(ServiceError::Other(
            "--gpu-pow-author requires a gpu-pow-v2 build and pow-local chain".into(),
        ));
    }
    #[cfg(feature = "gpu-pow-v2")]
    let _pow_finality_only = {
        let enabled = pow_finality_only_from_environment()?;
        if enabled && (!config.role.is_authority() || config.disable_grandpa) {
            return Err(ServiceError::Other(
                "EAGW_POW_FINALITY_ONLY requires an authority with GRANDPA enabled".into(),
            ));
        }
        if enabled && gpu_pow_author {
            return Err(ServiceError::Other(
                "--gpu-pow-author conflicts with EAGW_POW_FINALITY_ONLY".into(),
            ));
        }
        enabled
    };
    #[cfg(feature = "gpu-pow-v2")]
    let cpu_beneficiary = {
        #[cfg(feature = "svy-boot3-cpu")]
        {
            match std::env::var_os("SVY_BOOT3_CPU") {
                None => None,
                Some(v) if v == "1" && !config.role.is_authority() && !gpu_pow_author => {
                    let address = std::env::var("SVY_BOOT3_REWARD_ADDRESS").map_err(|_| {
                        ServiceError::Other("CPU authoring requires SVY_BOOT3_REWARD_ADDRESS to be a valid SVY address".into())
                    })?;
                    let beneficiary = crate::svy_address::decode(&address).map_err(|error| {
                        ServiceError::Other(format!("invalid SVY_BOOT3_REWARD_ADDRESS: {error}"))
                    })?;
                    Some(eagle_runtime::PowBeneficiaryV1::new(beneficiary).encode_payload())
                }
                _ => return Err(ServiceError::Other("CPU authoring requires a dedicated non-voting full node".into())),
            }
        }
        #[cfg(not(feature = "svy-boot3-cpu"))]
        { None::<Vec<u8>> }
    };
    #[cfg(feature = "gpu-pow-v2")]
    let pow_authoring = if gpu_pow_author {
        #[cfg(not(feature = "svy-matrix-live"))]
        use sp_core::crypto::Ss58Codec;

        let address = gpu_pow_beneficiary.as_deref().expect("paired option checked above");
        #[cfg(feature = "svy-matrix-live")]
        let beneficiary = crate::svy_address::decode(address).map_err(ServiceError::Other)?;
        #[cfg(not(feature = "svy-matrix-live"))]
        let beneficiary = eagle_runtime::AccountId::from_ss58check(address)
        .map_err(|error| {
            ServiceError::Other(format!(
                "invalid --gpu-pow-beneficiary SS58 account: {error}"
            ))
        })?;
        let beneficiary = eagle_runtime::PowBeneficiaryV1::new(beneficiary).encode_payload();
        let budget = pow_authoring_budget_from_environment()?;
        let runner = crate::active_pow_mining::LocalGpuPowRunner::from_environment()
            .map_err(ServiceError::Other)?;
        Some((budget, runner, beneficiary))
    } else {
        None
    };
    let sc_service::PartialComponents {
        client,
        backend,
        mut task_manager,
        import_queue,
        keystore_container,
        select_chain,
        transaction_pool,
        other: (block_import, grandpa_link, mut telemetry),
    } = new_partial(&config)?;
    #[cfg(feature = "gpu-pow-v2")]
    if let Some((budget, runner, _)) = &pow_authoring {
        runner
            .reserve_authoring_session(
                budget.import_limit,
                budget.attempt_limit,
                budget.max_duration,
            )
            .map_err(ServiceError::Other)?;
    }
    let mut net_config = sc_network::config::FullNetworkConfiguration::<
        Block,
        <Block as sp_runtime::traits::Block>::Hash,
        N,
    >::new(&config.network, config.prometheus_registry().cloned());
    let metrics = N::register_notification_metrics(config.prometheus_registry());

    let peer_store_handle = net_config.peer_store_handle();
    let grandpa_protocol_name = sc_consensus_grandpa::protocol_standard_name(
        &client
            .block_hash(0)
            .ok()
            .flatten()
            .expect("Genesis block exists; qed"),
        &config.chain_spec,
    );
    let (grandpa_protocol_config, grandpa_notification_service) =
        sc_consensus_grandpa::grandpa_peers_set_config::<_, N>(
            grandpa_protocol_name.clone(),
            metrics.clone(),
            peer_store_handle,
        );
    net_config.add_notification_protocol(grandpa_protocol_config);

    let warp_sync = Arc::new(sc_consensus_grandpa::warp_proof::NetworkProvider::new(
        backend.clone(),
        grandpa_link.shared_authority_set().clone(),
        Vec::default(),
    ));

    // Copy the SAME BasicQueue ingress before build_network takes the queue.
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    let mx10_import_queue = Arc::new(std::sync::Mutex::new(
        sc_consensus::import_queue::ImportQueue::service(&import_queue),
    ));

    let (network, system_rpc_tx, tx_handler_controller, sync_service) =
        sc_service::build_network(sc_service::BuildNetworkParams {
            config: &config,
            net_config,
            client: client.clone(),
            transaction_pool: transaction_pool.clone(),
            spawn_handle: task_manager.spawn_handle(),
            import_queue,
            block_announce_validator_builder: None,
            warp_sync_config: Some(WarpSyncConfig::WithProvider(warp_sync)),
            block_relay: None,
            metrics,
        })?;

    if config.offchain_worker.enabled {
        let offchain_workers =
            sc_offchain::OffchainWorkers::new(sc_offchain::OffchainWorkerOptions {
                runtime_api_provider: client.clone(),
                is_validator: config.role.is_authority(),
                keystore: Some(keystore_container.keystore()),
                offchain_db: backend.offchain_storage(),
                transaction_pool: Some(OffchainTransactionPoolFactory::new(
                    transaction_pool.clone(),
                )),
                network_provider: Arc::new(network.clone()),
                enable_http_requests: true,
                custom_extensions: |_| vec![],
            })?;
        task_manager.spawn_handle().spawn(
            "offchain-workers-runner",
            "offchain-worker",
            offchain_workers
                .run(client.clone(), task_manager.spawn_handle())
                .boxed(),
        );
    }

    let role = config.role;
    #[cfg(not(feature = "gpu-pow-v2"))]
    let force_authoring = config.force_authoring;
    #[cfg(not(feature = "gpu-pow-v2"))]
    let backoff_authoring_blocks: Option<()> = None;
    let name = config.network.node_name.clone();
    let enable_grandpa = !config.disable_grandpa;
    let prometheus_registry = config.prometheus_registry().cloned();

    // Expose proposal-building only for the pinned isolated MX10 spec and
    // explicit IPv4 localhost endpoints. Never register it on V2/Live/public RPC.
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    let mx10_rpc_context = match crate::gpu_pow_consensus::WorkAlgorithm::from_spec(config.chain_spec.as_ref()).map_err(ServiceError::Other)? {
        crate::gpu_pow_consensus::WorkAlgorithm::Mx10N16(context) => Some(context),
        _ => None,
    };
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    let mx10_template_rpc = mx10_rpc_context.is_some()
        && config.rpc.addr.as_ref().is_some_and(|endpoints| !endpoints.is_empty()
            && endpoints.iter().all(|endpoint| endpoint.listen_addr.ip()
                == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)));

    let rpc_extensions_builder = {
        #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
        let mx10_import_queue = mx10_import_queue.clone();
        let client = client.clone();
        let pool = transaction_pool.clone();

        Box::new(move |_| {
            let deps = crate::rpc::FullDeps {
                client: client.clone(),
                pool: pool.clone(),
            };
            let mut module = crate::rpc::create_full(deps)?;
            #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
            if mx10_template_rpc {
                let context = mx10_rpc_context.expect("checked isolated RPC context");
                crate::rpc::register_mx10_template(&mut module, client.clone(), pool.clone(), context)?;
                crate::rpc::register_mx10_import(&mut module, client.clone(), mx10_import_queue.clone(), context)?;
            }
            Ok(module)
        })
    };

    let _rpc_handlers = sc_service::spawn_tasks(sc_service::SpawnTasksParams {
        network: Arc::new(network.clone()),
        client: client.clone(),
        keystore: keystore_container.keystore(),
        task_manager: &mut task_manager,
        transaction_pool: transaction_pool.clone(),
        rpc_builder: rpc_extensions_builder,
        backend,
        system_rpc_tx,
        tx_handler_controller,
        sync_service: sync_service.clone(),
        config,
        telemetry: telemetry.as_mut(),
    })?;

    #[cfg(not(feature = "gpu-pow-v2"))]
    if role.is_authority() {
        #[cfg(not(feature = "incinerator-rehearsal"))]
        let work_provider = crate::eagw_work_provider::LocalGpuWorkProvider::from_environment()
            .map_err(ServiceError::Other)?;
        let proposer_factory = sc_basic_authorship::ProposerFactory::new(
            task_manager.spawn_handle(),
            client.clone(),
            transaction_pool.clone(),
            prometheus_registry.as_ref(),
            telemetry.as_ref().map(|x| x.handle()),
        );
        #[cfg(not(feature = "incinerator-rehearsal"))]
        let proposer_factory = crate::gpu_work_authoring::WorkEnvironment {
            inner: proposer_factory,
            work: work_provider,
        };
        #[cfg(not(feature = "incinerator-rehearsal"))]
        let block_import = crate::gpu_work_authoring::LocalWorkDigestSplit {
            inner: block_import,
        };

        let slot_duration = sc_consensus_aura::slot_duration(&*client)?;

        let aura = sc_consensus_aura::start_aura::<AuraPair, _, _, _, _, _, _, _, _, _, _>(
            StartAuraParams {
                slot_duration,
                client,
                select_chain,
                block_import,
                proposer_factory,
                create_inherent_data_providers: move |_, ()| async move {
                    let timestamp = sp_timestamp::InherentDataProvider::from_system_time();

                    let slot =
						sp_consensus_aura::inherents::InherentDataProvider::from_timestamp_and_slot_duration(
							*timestamp,
							slot_duration,
						);

                    Ok((slot, timestamp))
                },
                force_authoring,
                backoff_authoring_blocks,
                keystore: keystore_container.keystore(),
                sync_oracle: sync_service.clone(),
                justification_sync_link: sync_service.clone(),
                block_proposal_slot_portion: SlotProportion::new(2f32 / 3f32),
                max_block_proposal_slot_portion: None,
                telemetry: telemetry.as_ref().map(|x| x.handle()),
                compatibility_mode: Default::default(),
            },
        )?;

        // the AURA authoring task is considered essential, i.e. if it
        // fails we take down the service with it.
        task_manager
            .spawn_essential_handle()
            .spawn_blocking("aura", Some("block-authoring"), aura);
    }

    #[cfg(feature = "gpu-pow-v2")]
    if let Some(beneficiary) = pow_authoring.as_ref().map(|(_, _, b)| b.clone()).or(cpu_beneficiary) {
        let genesis = client
            .block_hash(0)
            .map_err(|error| ServiceError::Other(format!("cannot read PoW genesis: {error}")))?
            .ok_or_else(|| ServiceError::Other("missing PoW genesis".into()))?;
        let algorithm =
            crate::active_pow_consensus::DynamicDifficultyAlgorithm::new(genesis, client.clone());
        let import_inherent = move |_, ()| async move {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                sp_timestamp::InherentDataProvider::from_system_time(),
            )
        };
        let author_import = sc_consensus_pow::PowBlockImport::new(
            block_import.clone(),
            client.clone(),
            algorithm.clone(),
            0,
            select_chain.clone(),
            import_inherent,
        );
        let proposer_factory = sc_basic_authorship::ProposerFactory::new(
            task_manager.spawn_handle(),
            client.clone(),
            transaction_pool.clone(),
            prometheus_registry.as_ref(),
            telemetry.as_ref().map(|value| value.handle()),
        );
        let mining_inherent = move |_, ()| async move {
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                sp_timestamp::InherentDataProvider::from_system_time(),
            )
        };
        let (mining_handle, mining_worker) = sc_consensus_pow::start_mining_worker(
            Box::new(author_import),
            client.clone(),
            select_chain.clone(),
            algorithm,
            proposer_factory,
            sync_service.clone(),
            sync_service.clone(),
            Some(beneficiary),
            mining_inherent,
            Duration::from_secs(1),
            Duration::from_millis(eagle_runtime::SLOT_DURATION),
            Duration::from_secs(2),
        );
        task_manager.spawn_essential_handle().spawn_blocking(
            "pow-build-worker",
            Some("block-authoring"),
            mining_worker,
        );

        if let Some((authoring_budget, runner, _)) = pow_authoring {
        let supervisor = async move {
            let mut last_success_parent = None;
            let mut imported_count = 0_u64;
            let mut attempt_count = 0_u64;
            let session_deadline = Instant::now() + authoring_budget.max_duration;
            loop {
                if attempt_count >= authoring_budget.attempt_limit
                    || Instant::now() >= session_deadline
                {
                    runner
                        .record_authoring_stopped(
                            imported_count,
                            attempt_count,
                            "session budget exhausted",
                        )
                        .unwrap_or_else(|error| {
                            panic!("cannot persist bounded authoring stop: {error}")
                        });
                    eprintln!(
                        "PoW authoring paused after {attempt_count} attempts and {imported_count} imports"
                    );
                    futures::future::pending::<()>().await;
                }
                let Some((version, metadata)) = mining_handle.snapshot() else {
                    futures_timer::Delay::new(Duration::from_millis(50)).await;
                    continue;
                };
                // A replacement proposal can be published on the old best while the successful
                // import notification is still in flight. Do not spend GPU work or submit twice
                // on that parent; wait for a build based on the imported child.
                if last_success_parent == Some(metadata.best_hash) {
                    futures_timer::Delay::new(Duration::from_millis(50)).await;
                    continue;
                }
                attempt_count += 1;
                runner
                    .record_authoring_attempt(attempt_count)
                    .unwrap_or_else(|error| {
                        panic!("cannot persist PoW authoring attempt budget: {error}")
                    });
                let mined = runner
                    .mine(
                        genesis,
                        metadata.best_hash,
                        metadata.pre_hash,
                        metadata.difficulty,
                        session_deadline,
                    )
                    .unwrap_or_else(|error| {
                        runner
                            .record_authoring_stopped(
                                imported_count,
                                attempt_count,
                                &format!("bounded execution stopped: {error}"),
                            )
                            .unwrap_or_else(|record_error| {
                                panic!(
                                    "bounded PoW execution stopped: {error}; stop record failed: {record_error}"
                                )
                            });
                        panic!("bounded PoW execution stopped: {error}")
                    });
                // A worker may prove that a bounded nonce range had no
                // winner. Its runner advances the candidate cursor;
                // this is a normal mining miss, not an unknown submission.
                let Some(mined) = mined else {
                    continue;
                };
                runner
                    .record_submit_intent(&mined)
                    .unwrap_or_else(|error| panic!("cannot persist PoW submit intent: {error}"));
                match mining_handle
                    .submit_at_detailed(version, mined.seal.clone())
                    .await
                {
                    sc_consensus_pow::SubmitOutcome::Imported => {
                        runner
                            .record_submit_observed(&mined)
                            .unwrap_or_else(|error| {
                                panic!("PoW imported but outcome recording is unknown: {error}")
                            });
                        last_success_parent = Some(metadata.best_hash);
                        imported_count = imported_count.saturating_add(1);
                        if authoring_budget.import_limit == imported_count {
                            runner
                                .record_authoring_complete(imported_count, attempt_count)
                                .unwrap_or_else(|error| {
                                    panic!("cannot persist bounded authoring completion: {error}")
                                });
                            eprintln!(
                                "PoW authoring paused after bounded limit of {imported_count} imported blocks"
                            );
                            futures::future::pending::<()>().await;
                        }
                    }
                    sc_consensus_pow::SubmitOutcome::Stale => {
                        runner.record_submit_stale(&mined).unwrap_or_else(|error| {
                            panic!("cannot persist known-stale PoW submission: {error}")
                        });
                    }
                    sc_consensus_pow::SubmitOutcome::Rejected => {
                        let error = runner
                            .record_submit_unknown(&mined, "submission was definitively rejected");
                        panic!("PoW submission stopped: {error}");
                    }
                    sc_consensus_pow::SubmitOutcome::Unknown => {
                        let error =
                            runner.record_submit_unknown(&mined, "block import returned error");
                        panic!("PoW submission stopped: {error}");
                    }
                }
            }
        };
        task_manager.spawn_essential_handle().spawn_blocking(
            "pow-gpu-supervisor",
            Some("block-authoring"),
            supervisor,
        );
        } else {
            #[cfg(feature = "svy-boot3-cpu")]
            {
                use sp_blockchain::HeaderBackend;
                // A production miner must use boot3's existing public P2P identity.
                if !cfg!(feature = "svy-boot3-rehearsal") && network.local_peer_id().to_string() != "12D3KooWKBcH7DvCCkNAmjTMo4dB9dne1EuEBQrRMhBvFvrmHCjn" {
                    return Err(ServiceError::Other("boot3 peer ID mismatch".into()));
                }
                let author_client = client.clone();
                let work_export = match std::env::var_os("SVY_BOOT3_WORK_EXPORT_DIR") {
                    None => None,
                    Some(path) => {
                        let path = std::path::PathBuf::from(path);
                        if !path.is_absolute() ||
                            !std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
                            return Err(ServiceError::Other("GPU work export requires an absolute, existing directory".into()));
                        }
                        Some(path)
                    }
                };
                let gpu_accept = std::env::var_os("SVY_BOOT3_GPU_ACCEPT").is_some_and(|flag| flag == "1");
                let work = async move {
                    let mut last_parent = None;
                    let mut cursor = None;
                    let mut exported_pre_hash = None;
                    let mut gpu_wait: Option<(sp_core::H256, Instant)> = None;
                    loop {
                        futures_timer::Delay::new(Duration::from_millis(75)).await;
                        let Some((version, meta)) = mining_handle.snapshot() else { continue };
                        let head = author_client.info();
                        if head.best_hash != meta.best_hash || last_parent == Some(meta.best_hash) { continue }
                        if cfg!(feature = "svy-boot3-rehearsal") && !cfg!(feature = "svy-dead-mainnet-drill") && head.best_number >= 2 {
                            futures::future::pending::<()>().await;
                        }
                        let smw = crate::smw_activation::active(crate::smw_activation::scheduled(genesis), u64::from(head.best_number));
                        let challenge = if smw { crate::gpu_pow_mx10::challenge_from_pre_hash(genesis, meta.pre_hash) } else { crate::gpu_pow::challenge_from_pre_hash(genesis, meta.pre_hash) };
                        let target = crate::gpu_pow_consensus::target_for_difficulty(meta.difficulty).expect("valid consensus difficulty");
                        if let Some(directory) = &work_export {
                            if exported_pre_hash != Some(meta.pre_hash) {
                                let job = serde_json::json!({
                                    "genesis": format!("{genesis:?}"),
                                    "height": head.best_number + 1,
                                    "parentHash": format!("{:?}", meta.best_hash),
                                    "preHash": format!("{:?}", meta.pre_hash),
                                    "workAlgorithm": if smw { crate::chain_spec::MX10_WORK_ALGORITHM } else { "integer-matrix-2x2048x16" },
                                    "chainTarget": format!("{:?}", sp_core::H256::from(target)),
                                });
                                let temp = directory.join("job.json.next");
                                match std::fs::write(&temp, job.to_string())
                                    .and_then(|()| std::fs::rename(&temp, directory.join("job.json"))) {
                                    Ok(()) => exported_pre_hash = Some(meta.pre_hash),
                                    Err(error) => eprintln!("GPU work export unavailable: {error}"),
                                }
                            }
                            if gpu_accept {
                                // The relay writes a complete candidate with atomic rename. Only
                                // this local, private directory is used; the node also verifies
                                // the proof against its current pre-hash and consensus target.
                                let submitted = directory.join("seal.bin");
                                if let Ok(metadata) = std::fs::symlink_metadata(&submitted) {
                                    if metadata.is_file() && !metadata.file_type().is_symlink()
                                        && metadata.len() == crate::gpu_pow::CERTIFICATE_BYTES as u64 {
                                        if let Ok(proof) = std::fs::read(&submitted) {
                                            let _ = std::fs::remove_file(&submitted);
                                            let valid = if smw { crate::smw_activation::verify(genesis, meta.pre_hash, &proof, target) } else { crate::gpu_pow::verify_pre_hash(genesis, meta.pre_hash, &proof, target).is_ok() };
                                            if valid {
                                                match mining_handle.submit_at_detailed(version, proof).await {
                                                    sc_consensus_pow::SubmitOutcome::Imported => {
                                                        eprintln!("BOOT3_GPU_IMPORTED height={} parent={:?} pre_hash={:?}", head.best_number + 1, meta.best_hash, meta.pre_hash);
                                                        last_parent = Some(meta.best_hash);
                                                        futures_timer::Delay::new(Duration::from_secs(10)).await;
                                                        continue;
                                                    }
                                                    sc_consensus_pow::SubmitOutcome::Stale => continue,
                                                    outcome => panic!("GPU import stopped; inspect chain before resuming: {outcome:?}"),
                                                }
                                            }
                                        }
                                    }
                                }
                                let deadline = gpu_wait.filter(|(parent, _)| *parent == meta.best_hash)
                                    .map_or_else(|| Instant::now() + Duration::from_millis(750), |(_, deadline)| deadline);
                                gpu_wait = Some((meta.best_hash, deadline));
                                if Instant::now() < deadline {
                                    continue;
                                }
                            }
                        }
                        let start = cursor.filter(|(hash, _)| *hash == meta.pre_hash).map_or(0u64, |(_, n)| n);
                        let end = start.saturating_add(64);
                        cursor = Some((meta.pre_hash, end));
                        for nonce in start..end {
                            let Some(seal) = crate::smw_activation::candidate(smw, challenge, nonce, target) else { continue };
                            match mining_handle.submit_at_detailed(version, seal).await {
                                sc_consensus_pow::SubmitOutcome::Imported => {
                                    eprintln!("BOOT3_CPU_IMPORTED height={} parent={:?} pre_hash={:?} nonce={}",head.best_number+1,meta.best_hash,meta.pre_hash,nonce);
                                    last_parent = Some(meta.best_hash);
                                    futures_timer::Delay::new(Duration::from_secs(10)).await;
                                }
                                sc_consensus_pow::SubmitOutcome::Stale => (),
                                outcome => panic!("CPU import stopped; inspect chain before resuming: {outcome:?}"),
                            }
                            break;
                        }
                    }
                };
                task_manager.spawn_essential_handle().spawn_blocking("boot3-cpu-author",Some("block-authoring"),work);
            }
        }
    }

    if enable_grandpa {
        // if the node isn't actively participating in consensus then it doesn't
        // need a keystore, regardless of which protocol we use below.
        let keystore = if role.is_authority() {
            Some(keystore_container.keystore())
        } else {
            None
        };

        let grandpa_config = sc_consensus_grandpa::Config {
            // FIXME #1578 make this available through chainspec
            gossip_duration: Duration::from_millis(333),
            justification_generation_period: GRANDPA_JUSTIFICATION_PERIOD,
            name: Some(name),
            observer_enabled: false,
            keystore,
            local_role: role,
            telemetry: telemetry.as_ref().map(|x| x.handle()),
            protocol_name: grandpa_protocol_name,
        };

        // start the full GRANDPA voter
        // NOTE: non-authorities could run the GRANDPA observer protocol, but at
        // this point the full voter should provide better guarantees of block
        // and vote data availability than the observer. The observer has not
        // been tested extensively yet and having most nodes in a network run it
        // could lead to finality stalls.
        let grandpa_config = sc_consensus_grandpa::GrandpaParams {
            config: grandpa_config,
            link: grandpa_link,
            network,
            sync: Arc::new(sync_service),
            notification_service: grandpa_notification_service,
            // The upstream default deliberately stays two blocks behind best.
            // This closed PoW network uses an explicit one-block reorg guard:
            // finality follows best as closely as possible without immediately
            // finalizing the just-mined tip.
            voting_rule: sc_consensus_grandpa::VotingRulesBuilder::new()
                .add(sc_consensus_grandpa::BeforeBestBlockBy(1u32.into()))
                .build(),
            prometheus_registry,
            shared_voter_state: SharedVoterState::empty(),
            telemetry: telemetry.as_ref().map(|x| x.handle()),
            offchain_tx_pool_factory: OffchainTransactionPoolFactory::new(transaction_pool),
        };

        // the GRANDPA voter task is considered infallible, i.e.
        // if it fails we take down the service with it.
        task_manager.spawn_essential_handle().spawn_blocking(
            "grandpa-voter",
            None,
            sc_consensus_grandpa::run_grandpa_voter(grandpa_config)?,
        );
    }

    Ok(task_manager)
}

// Separate bounded offline author entry; no V2 authoring reinterpretation.
#[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
#[path = "mx10_author.rs"]
pub(crate) mod mx10_author;
