//! Connections view and probes.
//!
//! A connection is an identified account, its projects and the operations it
//! may perform. The view never exposes a secret: only the name of the
//! environment variable that holds one is reported.
use super::{ApiError, ApiResult, ApiState, Identity};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use layeragent_config::{Config, McpServer, Provider};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;

fn safe(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

pub(super) fn id(kind: &str, name: &str) -> String {
    format!("{kind}-{}", safe(name))
}

fn numeric_revision(revision: &str) -> i64 {
    i64::from_str_radix(revision.get(..8).unwrap_or("0"), 16).unwrap_or(0)
}

fn provider(provider: &Provider, name: &str, state: Option<&Value>, revision: i64) -> Value {
    let credential = match (&provider.key_env, &provider.oauth) {
        (Some(env), _) if std::env::var(env).is_err() => (
            "insufficient_permission",
            Some(format!("set {env} to authorize this account")),
        ),
        (None, Some(_)) => (
            "unknown",
            Some("OAuth token state is not inspected by the daemon".into()),
        ),
        (None, None) => (
            "insufficient_permission",
            Some("configure a credential reference (key_env or oauth)".into()),
        ),
        _ => ("configured", None),
    };
    let account = reqwest::Url::parse(&provider.base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string));
    let status = state
        .and_then(|s| s["last_probe"].as_str())
        .unwrap_or(credential.0);
    let detail = state
        .and_then(|s| s["last_probe"].as_str())
        .map(|_| "last probe recorded".to_string())
        .or(credential.1);
    json!({
        "id": id("provider", name),
        "kind": "provider",
        "name": name,
        "account": account,
        "projects": [],
        "operations": ["models", "inference"],
        "status": status,
        "revision": revision,
        "enabled": enabled(state, true),
        "generation": generation(state, 1),
        "detail": detail,
    })
}

fn mcp(server: &McpServer, name: &str, state: Option<&Value>, revision: i64) -> Value {
    let (status, detail): (&str, Option<String>) = if !server.enabled {
        (
            "disabled",
            Some("server is disabled in configuration".into()),
        )
    } else if server.grant.is_none() {
        (
            "insufficient_permission",
            Some("a restricted grant is required before MCP tools or resources are usable".into()),
        )
    } else {
        ("configured", None)
    };
    json!({
        "id": id("mcp", name),
        "kind": "mcp",
        "name": name,
        "account": server.url.as_deref().and_then(|url| reqwest::Url::parse(url).ok()).and_then(|url| url.host_str().map(str::to_string)),
        "projects": [],
        "operations": if server.grant.is_some() { json!(["tools", "resources"]) } else { json!(["tools"]) },
        "status": state.and_then(|s| s["last_probe"].as_str()).unwrap_or(status),
        "revision": revision,
        "enabled": enabled(state, server.enabled),
        "generation": generation(state, 1),
        "detail": detail,
    })
}

fn channel(
    channel: &layeragent_config::Channel,
    name: &str,
    state: Option<&Value>,
    revision: i64,
) -> Value {
    let (status, detail): (&str, Option<String>) = if std::env::var(&channel.token_env).is_err() {
        (
            "insufficient_permission",
            Some(format!(
                "set {} to authorize this channel",
                channel.token_env
            )),
        )
    } else {
        ("configured", None)
    };
    json!({
        "id": id("channel", name),
        "kind": "channel",
        "name": name,
        "account": channel.account,
        "projects": [channel.workspace.to_string_lossy()],
        "operations": ["receive", "send"],
        "status": state.and_then(|s| s["last_probe"].as_str()).unwrap_or(status),
        "revision": revision,
        "enabled": enabled(state, true),
        "generation": generation(state, 1),
        "detail": detail,
    })
}

fn enabled(state: Option<&Value>, fallback: bool) -> bool {
    state
        .and_then(|s| s["enabled"].as_bool())
        .unwrap_or(fallback)
}

fn generation(state: Option<&Value>, fallback: i64) -> i64 {
    state
        .and_then(|s| s["generation"].as_i64())
        .unwrap_or(fallback)
}

fn states(rows: &[Value]) -> BTreeMap<String, Value> {
    rows.iter()
        .filter_map(|row| Some((row["id"].as_str()?.to_string(), row.clone())))
        .collect()
}

pub(super) fn list(config: &Config, rows: &[Value]) -> Vec<Value> {
    let states = states(rows);
    let revision = numeric_revision(&config.revision());
    let mut items = Vec::new();
    for (name, provider_config) in &config.providers {
        let key = id("provider", name);
        items.push(provider(provider_config, name, states.get(&key), revision));
    }
    for (name, server) in &config.mcp {
        let key = id("mcp", name);
        items.push(mcp(server, name, states.get(&key), revision));
    }
    for (name, channel_config) in &config.channels {
        let key = id("channel", name);
        items.push(channel(channel_config, name, states.get(&key), revision));
    }
    items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    items
}

pub(super) async fn connections(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
) -> ApiResult<Json<Value>> {
    let config = s.engine.config().await;
    let store = s.engine.store(&i.scope).await?;
    Ok(Json(json!(list(
        &config,
        &store.all_connection_states().await?
    ))))
}

fn classify(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() || error.is_connect() {
        "unreachable"
    } else {
        "unknown"
    }
}

fn classify_status(status: StatusCode, expired: bool) -> &'static str {
    match status.as_u16() {
        401 if expired => "expired",
        401 | 403 => "insufficient_permission",
        429 | 400 | 404 | 405 => "rejected",
        500..=599 => "unreachable",
        _ => "reachable",
    }
}

async fn probe_provider(provider: &Provider) -> (&'static str, String) {
    if let Some(env) = &provider.key_env
        && std::env::var(env).is_err()
    {
        return (
            "insufficient_permission",
            format!("credential reference {env} is not set"),
        );
    }
    let expired = provider.oauth.is_some();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(error) => return ("unknown", error.to_string()),
    };
    let mut request = client.get(&provider.base_url);
    if let Some(env) = &provider.key_env
        && let Ok(key) = std::env::var(env)
    {
        request = request.bearer_auth(key);
    }
    match request.send().await {
        Ok(response) => {
            let status = classify_status(response.status(), expired);
            (
                status,
                format!("endpoint answered {}", response.status().as_u16()),
            )
        }
        Err(error) => (classify(&error), "endpoint did not answer".into()),
    }
}

async fn probe_mcp(server: &McpServer) -> (&'static str, String) {
    if let Some(command) = &server.command {
        let Some(binary) = command.first() else {
            return ("unknown", "empty command".into());
        };
        let resolved = if std::path::Path::new(binary).is_absolute() {
            std::path::Path::new(binary).is_file()
        } else {
            std::env::var_os("PATH").is_some_and(|paths| {
                std::env::split_paths(&paths).any(|dir| dir.join(binary).is_file())
            })
        };
        return if resolved {
            ("reachable", "command is available".into())
        } else {
            ("unreachable", "command is not available on PATH".into())
        };
    }
    let Some(url) = &server.url else {
        return ("unknown", "no command or URL configured".into());
    };
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(error) => return ("unknown", error.to_string()),
    };
    match client.get(url).send().await {
        Ok(response) => {
            let status = classify_status(response.status(), false);
            (
                status,
                format!("endpoint answered {}", response.status().as_u16()),
            )
        }
        Err(error) => (classify(&error), "endpoint did not answer".into()),
    }
}

pub(super) async fn action(
    State(s): State<ApiState>,
    axum::Extension(i): axum::Extension<Identity>,
    Path((connection, action)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    if !i.owner {
        return Err(ApiError::forbidden(anyhow::anyhow!(
            "owner access required to change connections"
        )));
    }
    let config = s.engine.config().await;
    let store = s.engine.store(&i.scope).await?;
    let mut items = list(&config, &store.all_connection_states().await?);
    let Some(item) = items
        .iter_mut()
        .find(|item| item["id"].as_str() == Some(connection.as_str()))
    else {
        return Err(ApiError::not_found(anyhow::anyhow!("connection not found")));
    };
    let kind = item["kind"].as_str().unwrap_or_default().to_string();
    let name = item["name"].as_str().unwrap_or_default().to_string();
    if kind.is_empty() || name.is_empty() {
        return Err(ApiError::not_found(anyhow::anyhow!(
            "connection has no identity"
        )));
    }
    match action.as_str() {
        "probe" => {
            let (status, detail) = match kind.as_str() {
                "provider" => match config.providers.get(&name) {
                    Some(provider) => probe_provider(provider).await,
                    None => ("unknown", "connection vanished from configuration".into()),
                },
                "mcp" => match config.mcp.get(&name) {
                    Some(server) => probe_mcp(server).await,
                    None => ("unknown", "connection vanished from configuration".into()),
                },
                "channel" => match config.channels.get(&name) {
                    Some(channel_config) => {
                        if std::env::var(&channel_config.token_env).is_err() {
                            (
                                "insufficient_permission",
                                format!(
                                    "set {} to authorize this channel",
                                    channel_config.token_env
                                ),
                            )
                        } else {
                            ("reachable", "credential reference is available".into())
                        }
                    }
                    None => ("unknown", "connection vanished from configuration".into()),
                },
                _ => ("unknown", "unsupported connection kind".into()),
            };
            store
                .set_connection_enabled(&connection, &kind, &name, true, Some(status))
                .await?;
            item["status"] = json!(status);
            item["detail"] = json!(detail);
        }
        "enable" | "disable" => {
            let enable = action == "enable";
            store
                .set_connection_enabled(&connection, &kind, &name, enable, None)
                .await?;
            let previous = item["status"].as_str().unwrap_or("configured").to_string();
            item["enabled"] = json!(enable);
            item["status"] = json!(if enable { previous } else { "disabled".into() });
        }
        "revoke" => {
            // Revocation advances the authority generation, invalidating every
            // in-flight observation of the previous grant. Running work is
            // stopped now, not merely refused at its next new operation.
            let generation = store.revoke_connection(&connection, &kind, &name).await?;
            s.engine.invalidate_authority()?;
            item["enabled"] = json!(false);
            item["status"] = json!("disabled");
            item["generation"] = json!(generation);
            item["detail"] = json!("revoked; re-enable to authorize a new generation");
        }
        other => {
            return Err(ApiError::with_status(
                StatusCode::BAD_REQUEST,
                anyhow::anyhow!("unknown connection action {other}"),
            ));
        }
    }
    Ok(Json(item.clone()))
}
