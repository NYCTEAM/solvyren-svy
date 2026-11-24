mod nvml_monitor;
mod stream_manager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| "local-miner".into());
    let endpoint = std::env::var("REPORT_URL").ok();
    tokio::select! {
        result = nvml_monitor::monitor_gpu_vram(node_id, endpoint) => result,
        result = tokio::signal::ctrl_c() => { result?; Ok(()) }
    }
}
