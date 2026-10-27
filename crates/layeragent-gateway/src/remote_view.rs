//! Remote job metadata for the operator interfaces.
//!
//! The daemon reads the public headers of the jobs that were submitted
//! through the encrypted bridge and the identity of the runner they were
//! addressed to. The payload of a job stays encrypted, the pairing keys stay
//! on the owner's machine, and the browser never contacts the coordinator.
use super::{ApiResult, ApiState};
use axum::{Json, extract::State};
use layeragent_remote::{ClientConfig, JobEnvelope};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Maximum number of submissions that are inspected in one request.
const MAX_SUBMISSIONS: usize = 200;
/// Maximum size of one submission file.
const MAX_SUBMISSION_BYTES: u64 = 1_048_576;

/// Reads the public metadata of the remote jobs of this profile.
pub(super) async fn remote_jobs(State(s): State<ApiState>) -> ApiResult<Json<Value>> {
    let config = s.engine.config().await;
    let Some(bridge) = config.remote.clone() else {
        return Ok(Json(
            json!({"configured": false, "jobs": [], "runner": Value::Null}),
        ));
    };
    let Ok(client) = load_client(&bridge.client) else {
        return Ok(Json(json!({
            "configured": true,
            "jobs": [],
            "runner": Value::Null,
            "detail": "the bridge configuration could not be read",
        })));
    };
    let Some(directory) = client.keys.parent().map(Path::to_path_buf) else {
        return Ok(Json(json!({
            "configured": true,
            "jobs": [],
            "runner": Value::Null,
            "detail": "the bridge has no local state directory",
        })));
    };
    let jobs = submissions(&directory);
    // The runner identity is the one the most recent job was addressed to.
    let runner = jobs
        .iter()
        .max_by_key(|job| job["issued_at_ms"].as_i64().unwrap_or(0))
        .map(|job| {
            json!({
                "runner": job["runner"],
                "device": job["device"],
                "account": job["account"],
                "project": job["project"],
                "policy_revision": job["policy_revision"],
                "operations": job["operations"],
            })
        })
        .unwrap_or(Value::Null);
    Ok(Json(json!({
        "configured": true,
        "jobs": jobs,
        "runner": runner,
        "job_count": jobs.len(),
    })))
}

/// Reads the owner client configuration of the bridge.
fn load_client(path: &Path) -> anyhow::Result<ClientConfig> {
    let raw = std::fs::read(path)?;
    anyhow::ensure!(
        raw.len() as u64 <= MAX_SUBMISSION_BYTES,
        "bridge configuration is too large"
    );
    Ok(serde_json::from_slice(&raw)?)
}

/// Collects the public headers of the saved submissions.
fn submissions(directory: &Path) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("submission-") || !name.ends_with(".json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.len() > MAX_SUBMISSION_BYTES {
            continue;
        }
        let Ok(raw) = std::fs::read(&path) else {
            continue;
        };
        let Ok(envelope) = serde_json::from_slice::<JobEnvelope>(&raw) else {
            continue;
        };
        let header = envelope.header;
        jobs.push(json!({
            "id": header.id,
            "account": header.account,
            "project": header.project,
            "runner": header.runner,
            "device": header.device,
            "epoch": header.epoch,
            "policy_revision": header.policy_revision,
            "operations": header.operations,
            "retry_safe": header.retry_safe,
            "issued_at_ms": header.issued_at_ms,
            "expires_at_ms": header.expires_at_ms,
        }));
        if jobs.len() >= MAX_SUBMISSIONS {
            break;
        }
    }
    jobs.sort_by_key(|job| job["issued_at_ms"].as_i64().unwrap_or(0));
    jobs
}
