// Independent diagnostic executable: preserves the running miner binary.
#[allow(dead_code)]
#[path = "../src/nvml_monitor.rs"]
mod nvml_monitor;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| "read-only-gpu-probe".into());
    let profiles = nvml_monitor::sample_gpu_once(node_id).await?;
    println!("{}", serde_json::to_string(&profiles)?);
    Ok(())
}
