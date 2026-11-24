#[derive(Debug, clap::Parser)]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Option<Subcommand>,

    #[clap(flatten)]
    pub run: sc_cli::RunCmd,

    /// Run the independent, bounded offline MX10 isolated-chain author, then exit.
    /// Does not start a network node or reuse --gpu-pow-author.
    #[arg(long)]
    pub mx10_author: bool,
    /// Number of consecutive blocks to author in this isolated session (1..=8).
    #[arg(long, default_value_t = 4)]
    pub mx10_author_blocks: u32,
    /// MX10 compute backend: auto prefers the unchanged CUDA kernel, cpu is explicit.
    #[arg(long, default_value = "auto", value_parser = ["auto", "gpu", "cpu"])]
    pub mx10_author_backend: String,
    /// New, non-existing session-output directory; required by --mx10-author.
    #[arg(long)]
    pub mx10_author_output: Option<std::path::PathBuf>,

    /// Enable the bounded local GPU PoW author on this node without granting
    /// it GRANDPA voting authority. Only available in gpu-pow-v2 builds.
    #[arg(long)]
    pub gpu_pow_author: bool,

    /// Reward address committed into locally authored PoW blocks (SVY-prefixed on SVY Live).
    /// Required with --gpu-pow-author and forbidden without it.
    #[arg(long, value_name = "ADDRESS")]
    pub gpu_pow_beneficiary: Option<String>,
}

#[derive(Debug, clap::Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum Subcommand {
    /// Key management cli utilities
    #[command(subcommand)]
    Key(sc_cli::KeySubcommand),

    /// Build a chain specification.
    /// DEPRECATED: `build-spec` command will be removed after 1/04/2026. Use `export-chain-spec`
    /// command instead.
    #[deprecated(
        note = "build-spec command will be removed after 1/04/2026. Use export-chain-spec command instead"
    )]
    BuildSpec(sc_cli::BuildSpecCmd),

    /// Export the chain specification.
    ExportChainSpec(sc_cli::ExportChainSpecCmd),

    /// Validate blocks.
    CheckBlock(sc_cli::CheckBlockCmd),

    /// Export blocks.
    ExportBlocks(sc_cli::ExportBlocksCmd),

    /// Export the state of a given block into a chain spec.
    ExportState(sc_cli::ExportStateCmd),

    /// Import blocks.
    ImportBlocks(sc_cli::ImportBlocksCmd),

    /// Remove the whole chain.
    PurgeChain(sc_cli::PurgeChainCmd),

    /// Revert the chain to a previous state.
    Revert(sc_cli::RevertCmd),

    /// Sub-commands concerned with benchmarking.
    #[cfg(not(windows))]
    #[command(subcommand)]
    Benchmark(frame_benchmarking_cli::BenchmarkCmd),

    /// Db meta columns information.
    ChainInfo(sc_cli::ChainInfoCmd),
}
