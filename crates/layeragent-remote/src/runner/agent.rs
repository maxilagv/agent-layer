//! Model execution stays on the paired computer, with a fresh scratch runtime.
use super::*;
use std::collections::BTreeSet;

pub(super) async fn execute_agent(
    task: &Task,
    config: &RunnerConfig,
    broker: Arc<FileBroker>,
    permit: Arc<Permit>,
) -> Result<Value> {
    let Task::Agent {
        prompt,
        model,
        apply_edits,
        apply_deletes,
        allow_shell,
    } = task
    else {
        anyhow::bail!("agent task required");
    };
    ensure!(
        prompt.len() <= 262144 && config.allowed_models.contains(model),
        "prompt exceeds limit or model not locally approved"
    );
    let home = config
        .agent_home
        .as_ref()
        .context("agent model profile not locally configured")?;
    let grant = load_grant(&config.grant)?;
    ensure!(
        !home.canonicalize()?.starts_with(grant.root.canonicalize()?),
        "agent credentials cannot reside in the project"
    );
    let mut agent_config = layeragent_config::Config::load(home, "default")?;
    ensure!(
        config.agent_profile_revision.as_deref() == Some(agent_config.revision().as_str()),
        "local model profile changed; explicit policy review and pairing required"
    );
    agent_config.require_model_egress(model)?;
    let descriptor = agent_config
        .models
        .get(model)
        .context("model not configured")?
        .clone();
    agent_config.primary = model.clone();
    agent_config.fallbacks.clear();
    agent_config.models.retain(|alias, _| alias == model);
    agent_config
        .providers
        .retain(|alias, _| alias == &descriptor.provider);
    agent_config.privacy.allowed_providers = vec![descriptor.provider];
    agent_config.access.clear();
    agent_config.channels.clear();
    agent_config.hooks.clear();
    agent_config.tools.clear();
    agent_config.mcp.clear();
    agent_config.skills.clear();
    agent_config.auto_skills = false;
    agent_config.agents.enabled = false;
    agent_config.browser.enabled = false;
    agent_config.memory.embeddings = false;
    agent_config.max_rounds = agent_config.max_rounds.min(16);
    agent_config.max_input_tokens = agent_config.max_input_tokens.min(100000);
    agent_config.max_output_tokens = agent_config.max_output_tokens.min(4096);
    agent_config.policy = layeragent_config::Policy {
        network: false,
        write: true,
        shell: *allow_shell,
        timeout_secs: config.limits.execution_secs,
        max_output_bytes: 131072,
        allowed_tools: [
            "read_file",
            "list_files",
            "search_files",
            "write_file",
            "edit_file",
        ]
        .into_iter()
        .map(String::from)
        .chain(allow_shell.then(|| "shell".to_string()))
        .collect(),
        approval_tools: vec![],
        limits: layeragent_config::Limits {
            cgroup: true,
            memory_bytes: Some(config.limits.memory_bytes),
            max_pids: Some(config.limits.max_pids),
            cpu_percent: Some(u32::from(config.limits.cpu_percent)),
            ..Default::default()
        },
        ..Default::default()
    };
    let scratch = tempfile::tempdir_in(&config.state)?;
    let workspace = scratch.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let original = snapshot_project(&broker, &grant, &workspace, &permit)?;
    let runtime = scratch.path().join("runtime");
    permit.check()?;
    let engine = layeragent_core::Engine::with_components(
        runtime,
        "default".into(),
        agent_config,
        Arc::new(layeragent_providers::Router::new()?),
        Arc::new(NativeSandbox::new(std::env::current_exe()?)),
    )?;
    let result = engine
        .run(
            layeragent_protocol::Scope::local(),
            layeragent_protocol::RunRequest {
                session_id: None,
                workspace: Some(workspace.to_string_lossy().into_owned()),
                prompt: prompt.clone(),
                model: Some(model.clone()),
                effort: Default::default(),
                bridge: false,
                mode: Default::default(),
                execution: Default::default(),
                cooperative: None,
            },
            permit.cancel.clone(),
        )
        .await;
    let shutdown = engine.shutdown_gracefully().await;
    let result = result?;
    shutdown?;
    permit.check()?;
    let read_grant = McpGrant {
        root: workspace.clone(),
        tools: layeragent_policy::file_catalog()
            .into_iter()
            .filter_map(|tool| {
                let name = tool["name"].as_str()?.to_string();
                (layeragent_policy::operation(&name).ok()? == Operation::Read).then(|| {
                    (
                        name,
                        layeragent_policy::ToolGrant {
                            operations: BTreeSet::from([Operation::Read]),
                            destinations: BTreeSet::from([".".into()]),
                        },
                    )
                })
            })
            .collect(),
        ..grant.clone()
    };
    let output_broker = FileBroker::open(read_grant.clone())?;
    let inspection = tempfile::tempdir_in(&config.state)?;
    let current = snapshot_project(&output_broker, &read_grant, inspection.path(), &permit)?;
    let mut changes = Vec::new();
    let mut bytes = 0usize;
    for (path, hash) in &current {
        if original.get(path) == Some(hash) {
            continue;
        }
        let data = output_broker.read_bytes_checked(&grant.scope, path, || permit.check())?;
        let content = std::str::from_utf8(&data)
            .context("UTF-8 change required")?
            .to_owned();
        bytes += content.len();
        ensure!(bytes <= 262144, "agent change set exceeds limit");
        changes.push(json!({"path":path,"content":content,"expected_sha256":original.get(path)}));
    }
    for path in original.keys() {
        if !current.contains_key(path) {
            changes.push(json!({"path":path,"delete":true,"expected_sha256":original[path]}));
        }
    }
    // Validate every proposed project effect before applying the first one.
    for change in &changes {
        let (tool, args) = effect(change);
        let enabled = if tool == "delete_file" {
            *apply_deletes
        } else {
            *apply_edits
        };
        if enabled {
            grant.authorize(&grant.scope, tool, &args)?;
        }
    }
    ensure!(
        serde_json::to_vec(&json!({"agent":result,"changes":changes}))?.len()
            <= MAX_ENVELOPE / 3 - 65536,
        "agent result exceeds encrypted limit before project commit"
    );
    let mut applied = Vec::new();
    for change in &changes {
        let (tool, args) = effect(change);
        let enabled = if tool == "delete_file" {
            *apply_deletes
        } else {
            *apply_edits
        };
        if enabled {
            permit.check()?;
            // Separate durable evidence for each external file effect. Unknown
            // jobs are never replayed to finish a partially applied change set.
            let effect = uuid::Uuid::new_v4().to_string();
            let audit = config.state.join("effects");
            layeragent_config::private_dir(&audit)?;
            crypto::private_write(
                &audit.join(format!("{effect}.intent.json")),
                &serde_json::to_vec(
                    &json!({"job":permit.job,"tool":tool,"path":change["path"],"arguments_sha256":layeragent_policy::digest(&serde_json::to_vec(&args)?)}),
                )?,
            )?;
            let receipt = broker.invoke_checked(&grant.scope, tool, &args, || permit.check())?;
            crypto::private_write(
                &audit.join(format!("{effect}.confirmed.json")),
                &serde_json::to_vec(&receipt)?,
            )?;
            applied.push(receipt);
        }
    }
    let response = json!({"agent":result,"changes":changes,"applied":applied,"apply_edits":apply_edits,"apply_deletes":apply_deletes});
    ensure!(
        serde_json::to_vec(&response)?.len() <= MAX_ENVELOPE / 3,
        "agent response exceeds encrypted result limit"
    );
    Ok(response)
}
fn effect(change: &Value) -> (&'static str, Value) {
    if change["delete"] == true {
        (
            "delete_file",
            json!({"path":change["path"],"expected_sha256":change["expected_sha256"]}),
        )
    } else {
        let mut args = json!({"path":change["path"],"content":change["content"]});
        if !change["expected_sha256"].is_null() {
            args["expected_sha256"] = change["expected_sha256"].clone();
        }
        ("write_file", args)
    }
}
