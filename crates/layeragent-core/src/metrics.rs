//! Local task measurements. Values are observations, not product claims.
use anyhow::Result;
use layeragent_storage::Storage;
use serde_json::{Value, json};

/// Peak resident set and cumulative CPU time of this daemon process.
pub fn process_usage() -> (u64, u64) {
    let rss = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| kb.saturating_mul(1024))
        .unwrap_or(0);
    let ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|text| {
            let tail = text.rsplit_once(") ")?.1;
            let fields = tail.split_whitespace().collect::<Vec<_>>();
            let utime = fields.get(11)?.parse::<u64>().ok()?;
            let stime = fields.get(12)?.parse::<u64>().ok()?;
            Some(utime + stime)
        })
        .unwrap_or(0);
    let hz = 100u64;
    (rss, ticks.saturating_mul(1000) / hz)
}

#[allow(clippy::too_many_arguments)]
pub async fn record(
    store: &Storage,
    task_id: &str,
    model: &str,
    usage: &layeragent_protocol::Usage,
    cost_usd: f64,
    provider_ms: u64,
    tool_ms: u64,
) -> Result<()> {
    let timing = store.task_timing(task_id).await?;
    let started = timing["started_at"].as_i64().unwrap_or(0);
    let created = timing["created_at"].as_i64().unwrap_or(started);
    let finished = timing["finished_at"].as_i64().unwrap_or(started);
    let queue = (started - created).max(0) as u64;
    let total = (finished - created).max(0) as u64;
    let first_content = timing["first_content_at"]
        .as_i64()
        .map(|at| (at - started).max(0) as u64);
    let network = timing["network_ms"].as_u64().unwrap_or(0);
    let engine = total.saturating_sub(provider_ms).saturating_sub(tool_ms);
    let (rss, cpu) = process_usage();
    let metrics: Value = json!({
        "model": model,
        "queue_ms": queue,
        "engine_ms": engine,
        "tool_ms": tool_ms,
        "provider_ms": provider_ms,
        "network_ms": network,
        "first_content_ms": first_content,
        "total_ms": total,
        "max_rss_bytes": rss,
        "cpu_ms": cpu,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "cost_usd": cost_usd,
    });
    store.record_task_metrics(task_id, &metrics).await?;
    Ok(())
}
