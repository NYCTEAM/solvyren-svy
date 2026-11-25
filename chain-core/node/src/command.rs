use crate::{chain_spec, cli::{Cli, Subcommand}, service};
#[cfg(all(not(windows), feature = "development-cli"))]
use crate::benchmarking::{inherent_benchmark_data, RemarkBuilder, TransferKeepAliveBuilder};
use eagle_runtime::Block;
#[cfg(all(not(windows), feature = "development-cli"))]
use eagle_runtime::EXISTENTIAL_DEPOSIT;
#[cfg(all(not(windows), feature = "development-cli"))]
use frame_benchmarking_cli::{BenchmarkCmd, ExtrinsicFactory, SUBSTRATE_REFERENCE_HARDWARE};
use sc_cli::SubstrateCli;
use sc_service::PartialComponents;
#[cfg(all(not(windows), feature = "development-cli"))]
use sp_keyring::Sr25519Keyring;

impl SubstrateCli for Cli {
    fn impl_name() -> String {
        "Solvyren".into()
    }

    fn impl_version() -> String {
        env!("SUBSTRATE_CLI_IMPL_VERSION").into()
    }

    fn description() -> String {
        env!("CARGO_PKG_DESCRIPTION").into()
    }

    fn author() -> String {
        env!("CARGO_PKG_AUTHORS").into()
    }

    fn support_url() -> String {
        "support.anonymous.an".into()
    }

    fn copyright_start_year() -> i32 {
        2017
    }

    fn load_spec(&self, id: &str) -> Result<Box<dyn sc_service::ChainSpec>, String> {
        #[cfg(feature = "incinerator-rehearsal")]
        if id == "dead-switch-drill" {
            let path = std::env::var_os("SVY_DEAD_SWITCH_LOCAL_SPEC")
                .ok_or_else(|| "Set the local DEAD switch drill spec path".to_string())?;
            let spec = chain_spec::ChainSpec::from_json_file(path.into())?;
            if spec.id() != "dev" || !spec.boot_nodes().is_empty() {
                return Err("The DEAD switch drill requires the isolated dev spec".into());
            }
            return Ok(Box::new(spec));
        }
        #[cfg(feature = "incinerator-rehearsal")]
        if id != "dev" {
            return Err("Incinerator rehearsal only accepts the built-in dev chain".into());
        }
        #[cfg(feature = "svy-matrix-live")]
        return chain_spec::load_svy_live_spec(id).map(|spec| Box::new(spec) as Box<dyn sc_service::ChainSpec>);
        #[cfg(not(feature = "svy-matrix-live"))]
        Ok(match id {
            #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
            "pow-mx10-n16-local" => {
                Box::new(chain_spec::pow_mx10_local_chain_spec()?)
            }
            #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
            "pow-matrix-block-rewards-local" => {
                Box::new(chain_spec::pow_matrix_block_rewards_local_chain_spec()?)
            }
            "dev" => Box::new(chain_spec::development_chain_spec()?),
            "pow-local" => Box::new(chain_spec::pow_local_chain_spec()?),
            "pow-rewards-local" => Box::new(chain_spec::pow_rewards_local_chain_spec()?),
            "pow-v3-rewards-local" => Box::new(chain_spec::pow_v3_rewards_local_chain_spec()?),
            #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
            "pow-v3-block-rewards-local" => {
                Box::new(chain_spec::pow_v3_block_rewards_local_chain_spec()?)
            }
            "" | "local" => Box::new(chain_spec::local_chain_spec()?),
            path => Box::new(chain_spec::ChainSpec::from_json_file(
                std::path::PathBuf::from(path),
            )?),
        })
    }
}

/// Parse and run command line arguments
pub fn run() -> sc_cli::Result<()> {
    let cli = Cli::from_args();
    #[cfg(feature = "mainnet-release")]
    if cli.run.shared_params.dev || cli.run.get_keyring().is_some() {
        return Err(sc_cli::Error::Input(
            "Development identities are unavailable in the Solvyren mainnet release".into(),
        ));
    }
    #[cfg(not(feature = "mainnet-release"))]
    let (gpu_pow_author, gpu_pow_beneficiary) = (cli.gpu_pow_author, cli.gpu_pow_beneficiary.clone());
    #[cfg(feature = "mainnet-release")]
    let (gpu_pow_author, gpu_pow_beneficiary) = (false, None);

    #[cfg(not(feature = "mainnet-release"))]
    if cli.mx10_author {
        if cli.subcommand.is_some() || gpu_pow_author || gpu_pow_beneficiary.is_some() {
            return Err(sc_cli::Error::Input("--mx10-author cannot be combined with subcommands or V2 GPU authoring".into()));
        }
        #[cfg(not(feature = "gpu-pow-matrix-block-rewards-dev"))]
        return Err(sc_cli::Error::Input("--mx10-author requires the isolated matrix test feature".into()));
        #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
        {
            let output = cli.mx10_author_output.clone().ok_or_else(||sc_cli::Error::Input("--mx10-author-output is required".into()))?;
            let options = service::mx10_author::Options { blocks: cli.mx10_author_blocks, backend: cli.mx10_author_backend.clone(), output };
            let runner = cli.create_runner(&cli.run)?;
            return runner.async_run(|config| service::mx10_author::run(config, options));
        }
    }

    match &cli.subcommand {
        Some(Subcommand::Key(cmd)) => cmd.run(&cli),
        #[allow(deprecated)]
        Some(Subcommand::BuildSpec(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run(config.chain_spec, config.network))
        }
        Some(Subcommand::CheckBlock(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::ExportChainSpec(cmd)) => {
            let chain_spec = cli.load_spec(&cmd.chain)?;
            cmd.run(chain_spec)
        }
        Some(Subcommand::ExportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, config.database), task_manager))
            })
        }
        Some(Subcommand::ExportState(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, config.chain_spec), task_manager))
            })
        }
        Some(Subcommand::ImportBlocks(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    import_queue,
                    ..
                } = service::new_partial(&config)?;
                Ok((cmd.run(client, import_queue), task_manager))
            })
        }
        Some(Subcommand::PurgeChain(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run(config.database))
        }
        Some(Subcommand::Revert(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.async_run(|config| {
                let PartialComponents {
                    client,
                    task_manager,
                    backend,
                    ..
                } = service::new_partial(&config)?;
                let aux_revert = Box::new(|client, _, blocks| {
                    sc_consensus_grandpa::revert(client, blocks)?;
                    Ok(())
                });
                Ok((cmd.run(client, backend, Some(aux_revert)), task_manager))
            })
        }
        #[cfg(all(not(windows), feature = "development-cli"))]
        Some(Subcommand::Benchmark(cmd)) => {
            let runner = cli.create_runner(cmd)?;

            runner.sync_run(|config| {
                // This switch needs to be in the client, since the client decides
                // which sub-commands it wants to support.
                match cmd {
                    BenchmarkCmd::Pallet(cmd) => {
                        if !cfg!(feature = "runtime-benchmarks") {
                            return Err(
                                "Runtime benchmarking wasn't enabled when building the node. \
							You can enable it with `--features runtime-benchmarks`."
                                    .into(),
                            );
                        }

                        cmd.run_with_spec::<sp_runtime::traits::HashingFor<Block>, ()>(Some(
                            config.chain_spec,
                        ))
                    }
                    BenchmarkCmd::Block(cmd) => {
                        let PartialComponents { client, .. } = service::new_partial(&config)?;
                        cmd.run(client)
                    }
                    #[cfg(not(feature = "runtime-benchmarks"))]
                    BenchmarkCmd::Storage(_) => Err(
                        "Storage benchmarking can be enabled with `--features runtime-benchmarks`."
                            .into(),
                    ),
                    #[cfg(feature = "runtime-benchmarks")]
                    BenchmarkCmd::Storage(cmd) => {
                        let PartialComponents {
                            client, backend, ..
                        } = service::new_partial(&config)?;
                        let db = backend.expose_db();
                        let storage = backend.expose_storage();
                        let shared_cache = backend.expose_shared_trie_cache();

                        cmd.run(config, client, db, storage, shared_cache)
                    }
                    BenchmarkCmd::Overhead(cmd) => {
                        let PartialComponents { client, .. } = service::new_partial(&config)?;
                        let ext_builder = RemarkBuilder::new(client.clone());

                        cmd.run(
                            config.chain_spec.name().into(),
                            client,
                            inherent_benchmark_data()?,
                            Vec::new(),
                            &ext_builder,
                            false,
                        )
                    }
                    BenchmarkCmd::Extrinsic(cmd) => {
                        let PartialComponents { client, .. } = service::new_partial(&config)?;
                        // Register the *Remark* and *TKA* builders.
                        let ext_factory = ExtrinsicFactory(vec![
                            Box::new(RemarkBuilder::new(client.clone())),
                            Box::new(TransferKeepAliveBuilder::new(
                                client.clone(),
                                Sr25519Keyring::Alice.to_account_id(),
                                EXISTENTIAL_DEPOSIT,
                            )),
                        ]);

                        cmd.run(client, inherent_benchmark_data()?, Vec::new(), &ext_factory)
                    }
                    BenchmarkCmd::Machine(cmd) => {
                        cmd.run(&config, SUBSTRATE_REFERENCE_HARDWARE.clone())
                    }
                }
            })
        }
        Some(Subcommand::ChainInfo(cmd)) => {
            let runner = cli.create_runner(cmd)?;
            runner.sync_run(|config| cmd.run::<Block>(&config))
        }
        None => {
            let network = &cli.run.network_params;
            let use_mainnet_tcp_default = network.listen_addr.is_empty();
            let p2p_port = network.port.unwrap_or(30333);
            let runner = cli.create_runner(&cli.run)?;
            runner.run_node_until_exit(|mut config| async move {
                if use_mainnet_tcp_default && config.chain_spec.id() == chain_spec::SVY_MATRIX_LIVE_ID {
                    config.network.listen_addresses = [
                        format!("/ip6/::/tcp/{p2p_port}"),
                        format!("/ip4/0.0.0.0/tcp/{p2p_port}"),
                    ].into_iter().map(|address| address.parse().expect("valid mainnet TCP address")).collect();
                }
                match config.network.network_backend {
                    sc_network::config::NetworkBackendType::Libp2p => {
                        service::new_full::<
                            sc_network::NetworkWorker<
                                eagle_runtime::opaque::Block,
                                <eagle_runtime::opaque::Block as sp_runtime::traits::Block>::Hash,
                            >,
                        >(
                            config, gpu_pow_author, gpu_pow_beneficiary.clone()
                        )
                        .map_err(sc_cli::Error::Service)
                    }
                    sc_network::config::NetworkBackendType::Litep2p => {
                        service::new_full::<sc_network::Litep2pNetworkBackend>(
                            config,
                            gpu_pow_author,
                            gpu_pow_beneficiary.clone(),
                        )
                        .map_err(sc_cli::Error::Service)
                    }
                }
            })
        }
    }
}
