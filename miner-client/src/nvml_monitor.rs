use anyhow::{bail, Context, Result};
use nvml_wrapper::{enum_wrappers::device::TemperatureSensor, Nvml};
use serde::Serialize;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time::{interval, MissedTickBehavior};

/// Bytes, degrees Celsius and milliwatts; UUID distinguishes cards in one node.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareProfile {
    pub node_id: String,
    pub gpu_uuid: String,
    pub model: String,
    pub total_vram_bytes: u64,
    pub free_vram_bytes: u64,
    pub temperature_c: Option<u32>,
    pub power_mw: Option<u32>,
    pub gpu_utilization_percent: Option<u32>,
    pub sampled_at_unix_ms: u64,
}

fn selected_gpu_uuid() -> Result<Option<String>> {
    match std::env::var("GPU_UUID") {
        Ok(uuid) => {
            if !uuid.starts_with("GPU-")
                || uuid.len() <= 4
                || uuid.len() > 80
                || uuid.chars().any(char::is_whitespace)
            {
                bail!("GPU_UUID must be a physical NVIDIA GPU UUID beginning with GPU-");
            }
            Ok(Some(uuid))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context("GPU_UUID is not valid Unicode"),
    }
}

fn initialize_nvml() -> Result<Nvml> {
    let nvml =
        Nvml::init().context("NVML unavailable: install NVIDIA driver or use an NVIDIA GPU")?;
    if nvml.device_count()? == 0 {
        bail!("No NVIDIA GPUs found");
    }
    Ok(nvml)
}

fn sample(nvml: &Nvml, node_id: &str, selected_uuid: Option<&str>) -> Result<Vec<HardwareProfile>> {
    let mut profiles = Vec::new();
    for index in 0..nvml.device_count()? {
        // A failed card does not suppress reports from the other cards.
        let card = (|| -> Result<Option<HardwareProfile>> {
            let device = nvml.device_by_index(index)?;
            let uuid = device.uuid()?;
            if selected_uuid.is_some_and(|selected| selected != uuid) {
                return Ok(None);
            }
            let memory = device.memory_info()?;
            Ok(Some(HardwareProfile {
                node_id: node_id.into(),
                gpu_uuid: uuid,
                model: device.name()?,
                total_vram_bytes: memory.total,
                free_vram_bytes: memory.free,
                temperature_c: device.temperature(TemperatureSensor::Gpu).ok(),
                power_mw: device.power_usage().ok(),
                gpu_utilization_percent: device.utilization_rates().ok().map(|rates| rates.gpu),
                sampled_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)?
                    .as_millis()
                    .try_into()?,
            }))
        })();
        match card {
            Ok(Some(profile)) => profiles.push(profile),
            Ok(None) => {}
            Err(error) => eprintln!("GPU {index} sampling failed: {error:#}"),
        }
    }
    if profiles.is_empty() {
        bail!("No readable matching NVIDIA GPU; check GPU_UUID and driver. Monitor stopped without retry loop");
    }
    Ok(profiles)
}

/// Read once using NVML only. Does not upload, register or execute GPU workloads.
pub async fn sample_gpu_once(node_id: String) -> Result<Vec<HardwareProfile>> {
    let selected_uuid = selected_gpu_uuid()?;
    tokio::task::spawn_blocking(move || {
        let nvml = initialize_nvml()?;
        sample(&nvml, &node_id, selected_uuid.as_deref())
    })
    .await?
}

/// Immediate first report, then every 10 seconds. Initialization failure returns once.
/// REPORT_URL is an ingestion gateway, not a raw Substrate JSON-RPC endpoint.
pub async fn monitor_gpu_vram(node_id: String, endpoint: Option<String>) -> Result<()> {
    let selected_uuid = selected_gpu_uuid()?;
    let nvml = Arc::new(tokio::task::spawn_blocking(initialize_nvml).await??);
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()?;
    let token = std::env::var("REPORT_TOKEN").ok();
    let mut ticker = interval(Duration::from_secs(10));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let nvml = Arc::clone(&nvml);
        let id = node_id.clone();
        let selected = selected_uuid.clone();
        let profiles =
            tokio::task::spawn_blocking(move || sample(&nvml, &id, selected.as_deref())).await??;
        if let Some(url) = &endpoint {
            let mut request = client.post(url).json(&profiles);
            if let Some(token) = &token {
                request = request.bearer_auth(token);
            }
            if let Err(error) = async {
                request.send().await?.error_for_status()?;
                Ok::<_, reqwest::Error>(())
            }
            .await
            {
                eprintln!("Heartbeat upload failed (next attempt in 10 seconds): {error}");
            }
        } else {
            println!("{}", serde_json::to_string(&profiles)?);
        }
    }
}
