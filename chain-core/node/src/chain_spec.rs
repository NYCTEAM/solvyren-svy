use eagle_runtime::WASM_BINARY;
use sc_service::ChainType;

/// Specialized `ChainSpec`. This is a specialization of the general Substrate ChainSpec type.
pub type ChainSpec = sc_service::GenericChainSpec;

/// Reserved spec ID for the opt-in isolated PoW import experiment.
pub const POW_LOCAL_ID: &str = "eagw_pow_v2_local";
pub const POW_REWARDS_LOCAL_ID: &str = "eagw_pow_rewards_dev_local";
pub const POW_V3_REWARDS_LOCAL_ID: &str = "eagw_pow_v3_rewards_dev_local";
pub const POW_V3_BLOCK_REWARDS_LOCAL_ID: &str = "eagw_pow_v3_block_rewards_dev_local";
pub const POW_MATRIX_BLOCK_REWARDS_LOCAL_ID: &str = "eagw_pow_matrix_block_rewards_dev_local";
#[cfg(feature = "svy-boot3-rehearsal")]
pub const SVY_MATRIX_LIVE_ID: &str = "svy-rc3-boot3-drill";
#[cfg(not(feature = "svy-boot3-rehearsal"))]
pub const SVY_MATRIX_LIVE_ID: &str = "solvyren-svy";

/// Read the same pinned bytes that will be passed to the chain-spec parser.
#[cfg(feature = "svy-matrix-live")]
pub fn load_svy_live_spec(path: &str) -> Result<ChainSpec, String> {
    use std::io::Read;

    #[cfg(all(feature = "svy-boot3-rehearsal", not(feature = "svy-dead-mainnet-drill")))]
    const EXPECTED_SHA256: &str = "06db521bb875435ccafa59631a27838401e917f10246828cf83b1de608734b2b";
    #[cfg(all(feature = "svy-transfer-fee-candidate", not(feature = "svy-boot3-rehearsal")))]
    const EXPECTED_SHA256: &str = "669568cfc22ef92da64eae6ae15b3116107f9ee58b751cd1cb886ec98291cc16";
    #[cfg(not(feature = "svy-transfer-fee-candidate"))]
    const EXPECTED_SHA256: &str = "6a6f4826f1790cde0f289f324c47db0a0a6e9984829345b55112c953d5c110e7";
    let file = std::fs::File::open(path).map_err(|error| format!("Open pinned Live spec: {error}"))?;
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("Live raw spec exceeds size limit".into());
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    #[cfg(not(feature = "svy-dead-mainnet-drill"))]
    if hex(&sp_core::hashing::sha2_256(&bytes)) != EXPECTED_SHA256 {
        return Err("Live raw spec SHA-256 mismatch".into());
    }
    let raw: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if raw["id"].as_str() != Some(SVY_MATRIX_LIVE_ID) || raw["chainType"].as_str() != Some(if cfg!(feature = "svy-boot3-rehearsal") { "Local" } else { "Live" }) {
        return Err("SVY Live chain identity required".into());
    }
    #[cfg(feature = "svy-dead-mainnet-drill")]
    if raw["protocolId"].as_str() != Some("svy-native-dead-drill")
        || !raw["bootNodes"].as_array().is_some_and(|nodes| nodes.is_empty())
    {
        return Err("An isolated native DEAD drill network is required".into());
    }
    let properties = &raw["properties"];
    if properties["tokenSymbol"].as_str() != Some("SVY")
        || properties["tokenDecimals"].as_u64() != Some(12)
        || properties["workAlgorithm"].as_str() != Some("integer-matrix-2x2048x16")
    {
        return Err("SVY native units and matrix work profile required".into());
    }
    let code = raw["genesis"]["raw"]["top"]["0x3a636f6465"]
        .as_str()
        .ok_or("Live genesis Runtime Wasm unavailable")?;
    #[cfg(feature = "mainnet-release")]
    {
        // This is the SHA-256 of the canonical hex code value in the pinned spec.
        // The full node executes the original Wasm from that spec and chain state.
        const EXPECTED_CODE_SHA256: &str = "c802b0b3d59c2f63e25a8aa2e97c6525748ca88a7f96446946cf20a5f0d9a19c";
        if hex(&sp_core::hashing::sha2_256(code.as_bytes())) != EXPECTED_CODE_SHA256 {
            return Err("Live genesis Runtime Wasm fingerprint mismatch".into());
        }
    }
    #[cfg(not(feature = "mainnet-release"))]
    {
        let wasm = WASM_BINARY.ok_or("Live Runtime Wasm unavailable")?;
        if code != format!("0x{}", hex(wasm)) {
            return Err("Live genesis Wasm differs from embedded Runtime".into());
        }
    }
    ChainSpec::from_json_bytes(bytes)
}

fn properties() -> sc_service::Properties {
    let mut properties = sc_service::Properties::new();
    properties.insert("tokenSymbol".into(), "EAG".into());
    properties.insert("tokenDecimals".into(), 12.into());
    properties.insert("ss58Format".into(), 42.into());
    properties
}

pub fn development_chain_spec() -> Result<ChainSpec, String> {
    let properties = properties();
    #[cfg(feature = "incinerator-rehearsal")]
    let properties = {
        let mut properties = properties;
        properties.insert("tokenSymbol".into(), "SVY".into());
        properties
    };
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Development")
    .with_properties(properties)
    .with_id("dev")
    .with_chain_type(ChainType::Development)
    .with_genesis_config_preset_name(sp_genesis_builder::DEV_RUNTIME_PRESET)
    .build())
}

pub fn local_chain_spec() -> Result<ChainSpec, String> {
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Local Testnet")
    .with_properties(properties())
    .with_id("local_testnet")
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Experimental PoW identity. Network and fork IDs are distinct; the reused
/// runtime genesis preset itself remains identical to local_testnet.
pub fn pow_local_chain_spec() -> Result<ChainSpec, String> {
    let mut p = properties();
    p.insert("consensusExperiment".into(), "gpu-pow-v2".into());
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Experimental PoW V2 Local")
    .with_properties(p)
    .with_id(POW_LOCAL_ID)
    .with_protocol_id(POW_LOCAL_ID)
    .with_fork_id(POW_LOCAL_ID)
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Zero-genesis experimental reward chain. It has a distinct network and
/// database identity and is accepted only by gpu-pow-rewards-dev binaries.
pub fn pow_rewards_local_chain_spec() -> Result<ChainSpec, String> {
    let mut p = properties();
    p.insert("consensusExperiment".into(), "gpu-pow-rewards-dev".into());
    p.insert(
        "emissionPolicy".into(),
        "gregorian-first-24-months-dev".into(),
    );
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Experimental PoW Rewards Dev Local")
    .with_properties(p)
    .with_id(POW_REWARDS_LOCAL_ID)
    .with_protocol_id(POW_REWARDS_LOCAL_ID)
    .with_fork_id(POW_REWARDS_LOCAL_ID)
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Version-3 GPU batch-search experiment. The V3 runtime Wasm has a distinct
/// runtime version and genesis code, so V2 reward databases cannot be reused.
pub fn pow_v3_rewards_local_chain_spec() -> Result<ChainSpec, String> {
    let mut p = properties();
    p.insert(
        "consensusExperiment".into(),
        "gpu-pow-v3-rewards-dev".into(),
    );
    p.insert(
        "emissionPolicy".into(),
        "gregorian-first-24-months-dev".into(),
    );
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Experimental PoW V3 Rewards Dev Local")
    .with_properties(p)
    .with_id(POW_V3_REWARDS_LOCAL_ID)
    .with_protocol_id(POW_V3_REWARDS_LOCAL_ID)
    .with_fork_id(POW_V3_REWARDS_LOCAL_ID)
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Isolated test identity. Its public development keys and deterministic
/// genesis funding must never be used as production authority or team keys.
#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
pub fn pow_v3_block_rewards_local_chain_spec() -> Result<ChainSpec, String> {
    let mut p = properties();
    p.insert(
        "consensusExperiment".into(),
        "gpu-pow-v3-block-rewards-dev".into(),
    );
    p.insert(
        "emissionPolicy".into(),
        "block-height-first24-tail-dev".into(),
    );
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Eagle AI Experimental Block Rewards Local")
    .with_properties(p)
    .with_id(POW_V3_BLOCK_REWARDS_LOCAL_ID)
    .with_protocol_id(POW_V3_BLOCK_REWARDS_LOCAL_ID)
    .with_fork_id(POW_V3_BLOCK_REWARDS_LOCAL_ID)
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Matrix work with current height-based issuance, for isolated research only.
/// Public development keys are not production authorities or team wallets.
#[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
pub fn pow_matrix_block_rewards_local_chain_spec() -> Result<ChainSpec, String> {
    let mut p = properties();
    p.insert("consensusExperiment".into(), "gpu-pow-matrix-block-rewards-dev".into());
    p.insert("emissionPolicy".into(), "block-height-first24-tail-dev".into());
    p.insert("workAlgorithm".into(), "integer-matrix-2x2048x16".into());
    Ok(ChainSpec::builder(
        WASM_BINARY.ok_or_else(|| "Development wasm not available".to_string())?,
        None,
    )
    .with_name("Solvyren Experimental Matrix Block Rewards Local")
    .with_properties(p)
    .with_id(POW_MATRIX_BLOCK_REWARDS_LOCAL_ID)
    .with_protocol_id(POW_MATRIX_BLOCK_REWARDS_LOCAL_ID)
    .with_fork_id(POW_MATRIX_BLOCK_REWARDS_LOCAL_ID)
    .with_chain_type(ChainType::Local)
    .with_genesis_config_preset_name(sp_genesis_builder::LOCAL_TESTNET_RUNTIME_PRESET)
    .build())
}

/// Unfrozen MX10 isolated test chain, separate from every V2 and production chain.
pub const POW_MX10_LOCAL_ID: &str = "svy_mx10_n16_isolated_local";
/// Exact identity of the currently deployed closed public-development chain.
/// Keep this explicit: matrix builds must not accept arbitrary external specs.
pub const POW_MX10_PUBLICDEV_ID: &str = "svy_mx10_publicdev_20261109";

/// Identities permitted to use the pinned isolated MX10 context.
/// Centralizing this prevents service, algorithm, and context guards drifting.
pub fn is_mx10_isolated_id(id: &str) -> bool {
    id == POW_MX10_LOCAL_ID || id == POW_MX10_PUBLICDEV_ID
}
/// Director-selected n=16 work identity, not a production or freeze declaration.
pub const MX10_WORK_ALGORITHM: &str = "25cdf8f5acd57e409d8324c71c15b271314e4effd3ef2ca75ff2c897d2275d17";

#[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
pub fn pow_mx10_local_chain_spec() -> Result<ChainSpec, String> {
    // Preserve the V2 builder verbatim. Bind this experimental identity into
    // genesis storage as well as spec properties: name/id alone do NOT change
    // the genesis header hash. No runtime difficulty or issuance changes.
    let base = pow_matrix_block_rewards_local_chain_spec()?;
    let mut raw: serde_json::Value = serde_json::from_str(&sc_service::ChainSpec::as_json(&base, true)?)
        .map_err(|e| e.to_string())?;
    raw["name"] = "Solvyren Unfrozen MX10 n16 Isolated Local".into();
    raw["id"] = POW_MX10_LOCAL_ID.into();
    raw["protocolId"] = POW_MX10_LOCAL_ID.into();
    raw["forkId"] = POW_MX10_LOCAL_ID.into();
    raw["properties"]["workAlgorithm"] = MX10_WORK_ALGORITHM.into();
    raw["properties"]["consensusExperiment"] = "gpu-pow-mx10-n16-isolated-unfrozen".into();
    let key = b"SVY-MX10-ISOLATED-WORKID\0".iter().map(|b| format!("{b:02x}")).collect::<String>();
    raw["genesis"]["raw"]["top"][format!("0x{key}")] = format!("0x{MX10_WORK_ALGORITHM}").into();
    ChainSpec::from_json_bytes(serde_json::to_vec(&raw).map_err(|e| e.to_string())?)
}
