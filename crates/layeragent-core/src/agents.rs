//! Child-agent orchestration.
//!
//! A user turn is the root of an [`AgentTree`] that owns the limits shared by every
//! descendant: a sticky token/cost budget, a concurrency semaphore and a kill switch.
//! Children run as ordinary engine turns in their own sessions, with a policy that can
//! only be narrower than their parent's, and are always reaped before the turn that
//! spawned them finishes.
use super::*;
use async_trait::async_trait;
use layeragent_config::AgentsConfig;
use layeragent_storage::AgentRow;
use layeragent_tools::AgentControl;
use std::sync::{
    OnceLock,
    atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

const RESULT_LIMIT: usize = 16 * 1024;
const REAP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15);

/// Tokens and cost shared by a whole agent tree. Exhaustion is sticky.
pub(crate) struct Budget {
    tokens: AtomicI64,
    cost_micro: AtomicI64,
    exhausted: AtomicBool,
}
impl Budget {
    pub(crate) fn new(tokens: Option<u64>, cost_usd: Option<f64>) -> Self {
        Self {
            tokens: AtomicI64::new(
                tokens.map_or(i64::MAX / 2, |t| t.min(i64::MAX as u64 / 2) as i64),
            ),
            cost_micro: AtomicI64::new(cost_usd.map_or(i64::MAX / 2, |c| (c * 1e6) as i64)),
            exhausted: AtomicBool::new(false),
        }
    }
    /// Records consumption; returns false once the tree is out of budget.
    pub(crate) fn charge(&self, tokens: u64, cost_usd: f64) -> bool {
        let tokens_left = self.tokens.fetch_sub(tokens as i64, Ordering::SeqCst) - tokens as i64;
        let cost_left = self
            .cost_micro
            .fetch_sub((cost_usd * 1e6) as i64, Ordering::SeqCst)
            - (cost_usd * 1e6) as i64;
        if tokens_left <= 0 || cost_left <= 0 {
            self.exhausted.store(true, Ordering::SeqCst);
        }
        !self.exhausted()
    }
    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::SeqCst)
    }
}

pub(crate) struct AgentTree {
    pub(crate) budget: Budget,
    pub(crate) bridge_enabled: bool,
    pub(crate) effort: EffortLevel,
    pub(crate) mode: RunMode,
    pub(crate) execution: ExecutionMode,
    permits: Arc<Semaphore>,
    /// Cancelled when the tree runs out of budget; every descendant observes it.
    pub(crate) stop: CancellationToken,
    agents: Mutex<HashMap<String, Arc<AgentHandle>>>,
    spawned: AtomicUsize,
}
impl AgentTree {
    pub(crate) fn new(
        cfg: &AgentsConfig,
        bridge_enabled: bool,
        effort: EffortLevel,
        mode: RunMode,
        execution: ExecutionMode,
    ) -> Arc<Self> {
        Arc::new(Self {
            budget: Budget::new(cfg.tree_max_tokens, cfg.tree_max_cost_usd),
            bridge_enabled,
            effort,
            mode,
            execution,
            permits: Arc::new(Semaphore::new(cfg.max_concurrent)),
            stop: CancellationToken::new(),
            agents: Mutex::new(HashMap::new()),
            spawned: AtomicUsize::new(0),
        })
    }
    fn children_of(&self, turn: &str) -> Vec<Arc<AgentHandle>> {
        self.agents
            .lock()
            .map(|m| {
                m.values()
                    .filter(|h| h.parent_turn == turn)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub(crate) struct AgentHandle {
    id: String,
    name: String,
    parent_turn: String,
    /// Turn ids of every ancestor run, nearest last; used to authorise cancellation.
    ancestors: Vec<String>,
    status: Mutex<&'static str>,
    session_id: Mutex<Option<String>>,
    cancel: CancellationToken,
    done: watch::Sender<bool>,
    result: Mutex<Option<Value>>,
    join: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl AgentHandle {
    fn set_status(&self, status: &'static str) {
        if let Ok(mut s) = self.status.lock() {
            *s = status;
        }
    }
    fn status(&self) -> &'static str {
        self.status.lock().map_or("unknown", |s| *s)
    }
    fn snapshot(&self, with_result: bool) -> Value {
        let mut v = json!({"agent_id":self.id,"name":self.name,"status":self.status()});
        if with_result
            && let Ok(r) = self.result.lock()
            && let Some(r) = r.as_ref()
        {
            v["result"] = r.clone();
        }
        v
    }
}

/// What distinguishes one run's position in the tree.
pub(crate) struct AgentCtl {
    pub(crate) tree: Arc<AgentTree>,
    pub(crate) depth: usize,
    pub(crate) agent_id: Option<String>,
    pub(crate) writable: bool,
    pub(crate) shares_workspace: bool,
    pub(crate) inherited_effort: Option<EffortLevel>,
    ancestors: Vec<String>,
    bound: OnceLock<(String, String, CancellationToken)>,
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}
impl AgentCtl {
    pub(crate) fn root(
        cfg: &AgentsConfig,
        bridge: bool,
        effort: EffortLevel,
        mode: RunMode,
        execution: ExecutionMode,
    ) -> Arc<Self> {
        Arc::new(Self {
            tree: AgentTree::new(cfg, bridge, effort, mode, execution),
            depth: 0,
            agent_id: None,
            writable: true,
            shares_workspace: false,
            inherited_effort: None,
            ancestors: vec![],
            bound: OnceLock::new(),
            permit: Mutex::new(None),
        })
    }
    pub(crate) fn is_child(&self) -> bool {
        self.agent_id.is_some()
    }
    /// Called once the run knows its session, turn and cancellation token.
    pub(crate) fn bind(&self, session: &str, turn: &str, cancel: &CancellationToken) {
        let _ = self
            .bound
            .set((session.into(), turn.into(), cancel.clone()));
    }
    fn bound(&self) -> Result<&(String, String, CancellationToken)> {
        self.bound
            .get()
            .context("agent context is not bound to a run")
    }
    pub(crate) fn hold(&self, permit: OwnedSemaphorePermit) {
        if let Ok(mut p) = self.permit.lock() {
            *p = Some(permit);
        }
    }
    /// Cancels and reaps every child spawned by `turn`. The wait is deliberately not
    /// tied to any cancellation token so cleanup cannot be skipped.
    pub(crate) async fn reap(&self, turn: &str) {
        let children = self.tree.children_of(turn);
        for child in &children {
            child.cancel.cancel();
        }
        let joins: Vec<_> = children
            .iter()
            .filter_map(|c| c.join.lock().ok().and_then(|mut j| j.take()))
            .collect();
        let _ = tokio::time::timeout(REAP_DEADLINE, async {
            for join in joins {
                let _ = join.await;
            }
        })
        .await;
    }
}

/// Narrows the base configuration for one agent in the tree.
pub(crate) fn effective_config(base: Arc<Config>, ctl: &AgentCtl) -> Arc<Config> {
    if !ctl.is_child() {
        return base;
    }
    let mut c = (*base).clone();
    let a = &base.agents;
    c.policy.write = base.policy.write && ctl.writable;
    c.policy.network = false;
    c.max_rounds = c.max_rounds.min(a.child_max_rounds);
    c.max_output_tokens = c.max_output_tokens.min(a.child_max_output_tokens);
    c.auto_skills = false;
    c.agents.enabled = base.agents.enabled && ctl.depth < a.max_depth;
    Arc::new(c)
}

pub(crate) struct EngineAgents {
    pub(crate) engine: Arc<Engine>,
    pub(crate) ctl: Arc<AgentCtl>,
}

fn clip_text(text: &str) -> String {
    clip(text, RESULT_LIMIT)
}

#[async_trait]
impl AgentControl for EngineAgents {
    async fn spawn(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let cfg = self.engine.config().await.agents.clone();
        let (parent_session, parent_turn, parent_cancel) = self.ctl.bound()?.clone();
        let tree = self.ctl.tree.clone();
        ensure!(
            self.ctl.depth < cfg.max_depth,
            "delegation depth limit reached"
        );
        ensure!(!tree.budget.exhausted(), "agent tree budget exhausted");
        let task = args["task"].as_str().unwrap_or("").trim().to_string();
        ensure!(!task.is_empty(), "task required");
        let writable = args["writable"].as_bool().unwrap_or(false);
        let isolation = args["isolation"].as_str().unwrap_or("shared").to_string();
        ensure!(
            !writable || isolation == "worktree",
            "writable children require isolation=worktree so they cannot race the parent"
        );
        ensure!(
            !writable || ctx.config.policy.write,
            "this task is read-only; it cannot create writable children"
        );
        let model = args["model"].as_str().map(str::to_string);
        if let Some(m) = &model {
            ensure!(ctx.config.models.contains_key(m), "unknown model {m}");
        }
        ensure!(
            tree.spawned.fetch_add(1, Ordering::SeqCst) < cfg.max_children_per_turn,
            "child agent limit reached for this task"
        );
        let id = new_id();
        let workspace = if isolation == "worktree" {
            let dest = self.engine.state_home.join("agents").join(&id);
            layeragent_tools::git::worktree_add(ctx.workspace, &dest).await?;
            dest
        } else {
            ctx.workspace.to_path_buf()
        };
        let name: String = args["name"]
            .as_str()
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.chars().filter(|c| !c.is_control()).take(64).collect())
            .unwrap_or_else(|| format!("agent-{}", &id[..8]));
        let mut ancestors = self.ctl.ancestors.clone();
        ancestors.push(parent_turn.clone());
        let (done, _) = watch::channel(false);
        let handle = Arc::new(AgentHandle {
            id: id.clone(),
            name: name.clone(),
            parent_turn: parent_turn.clone(),
            ancestors: ancestors.clone(),
            status: Mutex::new("queued"),
            session_id: Mutex::new(None),
            cancel: CancellationToken::new(),
            done,
            result: Mutex::new(None),
            join: Mutex::new(None),
        });
        tree.agents
            .lock()
            .map_err(|_| anyhow::anyhow!("agent registry poisoned"))?
            .insert(id.clone(), handle.clone());
        let child_ctl = Arc::new(AgentCtl {
            tree: tree.clone(),
            depth: self.ctl.depth + 1,
            agent_id: Some(id.clone()),
            writable,
            shares_workspace: isolation == "shared",
            inherited_effort: Some(ctx.config.default_effort),
            ancestors,
            bound: OnceLock::new(),
            permit: Mutex::new(None),
        });
        ctx.store
            .agent_create(AgentRow {
                id: id.clone(),
                root_turn_id: root_turn(&self.ctl, &parent_turn),
                parent_turn_id: parent_turn.clone(),
                parent_agent_id: self.ctl.agent_id.clone(),
                name: name.clone(),
                task: clip(&task, 4000),
                depth: child_ctl.depth,
                workspace: workspace.to_string_lossy().into_owned(),
                isolation: isolation.clone(),
            })
            .await?;
        self.engine
            .emit(
                ctx.scope,
                ctx.store,
                &parent_session,
                Some(&parent_turn),
                "subagent_started",
                json!({"agent_id":id,"name":name,"depth":child_ctl.depth,"isolation":isolation,"writable":writable}),
            )
            .await?;
        let job = ChildJob {
            engine: self.engine.clone(),
            scope: ctx.scope.clone(),
            store: ctx.store.clone(),
            handle: handle.clone(),
            ctl: child_ctl,
            parent_cancel,
            parent_session,
            workspace,
            worktree_of: (isolation == "worktree").then(|| ctx.workspace.to_path_buf()),
            request: RunRequest {
                session_id: None,
                workspace: None,
                prompt: task,
                model,
                effort: ctx.config.default_effort,
                bridge: self.ctl.tree.bridge_enabled,
                mode: self.ctl.tree.mode,
                execution: ExecutionMode::Delegated,
                cooperative: None,
            },
            timeout: std::time::Duration::from_secs(cfg.child_timeout_secs),
        };
        let join = tokio::spawn(job.run());
        if let Ok(mut slot) = handle.join.lock() {
            *slot = Some(join);
        }
        Ok(json!({"agent_id":id,"name":name,"status":"queued"}))
    }

    async fn wait(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let tree = &self.ctl.tree;
        let handles: Vec<Arc<AgentHandle>> = match args["ids"].as_array() {
            Some(ids) if !ids.is_empty() => {
                let map = tree
                    .agents
                    .lock()
                    .map_err(|_| anyhow::anyhow!("registry poisoned"))?;
                ids.iter()
                    .filter_map(|id| id.as_str())
                    .map(|id| {
                        map.get(id)
                            .filter(|h| {
                                h.ancestors.iter().any(|a| a == ctx.turn_id)
                                    || h.parent_turn == ctx.turn_id
                            })
                            .cloned()
                            .with_context(|| format!("unknown agent {id}"))
                    })
                    .collect::<Result<_>>()?
            }
            _ => tree.children_of(ctx.turn_id),
        };
        ensure!(!handles.is_empty(), "no child agents to wait for");
        let cfg_timeout = self.engine.config().await.agents.child_timeout_secs;
        let timeout = args["timeout_secs"]
            .as_u64()
            .unwrap_or(cfg_timeout)
            .min(3600);
        // A waiting agent must not hold a concurrency slot its descendants need.
        let released = self.ctl.permit.lock().ok().and_then(|mut p| p.take());
        drop(released);
        let all_done = async {
            for h in &handles {
                let mut rx = h.done.subscribe();
                while !*rx.borrow_and_update() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            }
        };
        let outcome = tokio::select! {
            _ = ctx.cancel.cancelled() => Err(anyhow::anyhow!("wait cancelled")),
            _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => Ok(false),
            _ = all_done => Ok(true),
        };
        if self.ctl.is_child() {
            let reacquired = tokio::select! {
                _ = ctx.cancel.cancelled() => None,
                p = tree.permits.clone().acquire_owned() => p.ok(),
            };
            if let Some(p) = reacquired {
                self.ctl.hold(p);
            }
        }
        let complete = outcome?;
        Ok(json!({
            "complete": complete,
            "agents": handles.iter().map(|h| h.snapshot(true)).collect::<Vec<_>>()
        }))
    }

    async fn cancel(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let id = args["id"].as_str().context("id required")?;
        let handle = self
            .ctl
            .tree
            .agents
            .lock()
            .map_err(|_| anyhow::anyhow!("registry poisoned"))?
            .get(id)
            .cloned()
            .context("unknown agent")?;
        ensure!(
            handle.parent_turn == ctx.turn_id || handle.ancestors.iter().any(|a| a == ctx.turn_id),
            "only an ancestor can cancel an agent"
        );
        handle.cancel.cancel();
        Ok(json!({"agent_id":id,"status":"cancelling"}))
    }

    async fn list(&self, ctx: &ToolContext<'_>) -> Result<Value> {
        Ok(
            json!({"agents": self.ctl.tree.children_of(ctx.turn_id).iter().map(|h| h.snapshot(false)).collect::<Vec<_>>()}),
        )
    }
}

/// Turn id of the root of the tree this run belongs to.
fn root_turn(ctl: &AgentCtl, own_turn: &str) -> String {
    ctl.ancestors
        .first()
        .cloned()
        .unwrap_or_else(|| own_turn.to_string())
}

struct ChildJob {
    engine: Arc<Engine>,
    scope: Scope,
    store: Storage,
    handle: Arc<AgentHandle>,
    ctl: Arc<AgentCtl>,
    parent_cancel: CancellationToken,
    parent_session: String,
    workspace: PathBuf,
    worktree_of: Option<PathBuf>,
    request: RunRequest,
    timeout: std::time::Duration,
}

impl ChildJob {
    async fn run(self) {
        let ChildJob {
            engine,
            scope,
            store,
            handle,
            ctl,
            parent_cancel,
            parent_session,
            workspace,
            worktree_of,
            mut request,
            timeout,
        } = self;
        let cancel = handle.cancel.clone();
        // Cancellation flows down from the parent turn and from the tree kill switch.
        let watcher = {
            let (own, parent, stop) = (cancel.clone(), parent_cancel, ctl.tree.stop.clone());
            tokio::spawn(async move {
                tokio::select! { _ = parent.cancelled() => own.cancel(), _ = stop.cancelled() => own.cancel(), _ = own.cancelled() => {} }
            })
        };
        let forwarder = tokio::spawn(forward_events(
            engine.clone(),
            scope.clone(),
            store.clone(),
            handle.clone(),
            parent_session.clone(),
        ));
        let permit = tokio::select! {
            _ = cancel.cancelled() => None,
            p = ctl.tree.permits.clone().acquire_owned() => p.ok(),
        };
        let (status, text, usage, error) = match permit {
            None => (
                "cancelled",
                None,
                None,
                Some("cancelled before start".to_string()),
            ),
            Some(permit) => {
                ctl.hold(permit);
                handle.set_status("running");
                let _ = store
                    .agent_update(&handle.id, "running", None, None, None)
                    .await;
                request.workspace = Some(workspace.to_string_lossy().into_owned());
                let run = engine.run_inner(
                    scope.clone(),
                    request,
                    cancel.clone(),
                    Some(ctl.clone()),
                    None,
                );
                let outcome = tokio::select! {
                    r = run => r,
                    _ = tokio::time::sleep(timeout) => { cancel.cancel(); Err(anyhow::anyhow!("child agent timed out")) }
                };
                match outcome {
                    Ok(r) => (
                        "completed",
                        Some(r.text),
                        serde_json::to_value(&r.usage).ok(),
                        None,
                    ),
                    Err(e) if cancel.is_cancelled() => {
                        ("cancelled", None, None, Some(e.to_string()))
                    }
                    Err(e) => ("failed", None, None, Some(e.to_string())),
                }
            }
        };
        let mut result = json!({"text": text.as_deref().map(clip_text), "error": error});
        if let Some(repo) = &worktree_of {
            match layeragent_tools::git::worktree_patch(&workspace).await {
                Ok(patch) if !patch.is_empty() => {
                    let artifact = store
                        .put_artifact(&scope.principal, patch.clone().into_bytes())
                        .await
                        .ok();
                    result["patch"] = json!({"artifact":artifact,"preview":clip(&patch,4096),"bytes":patch.len()});
                }
                Ok(_) => {}
                Err(e) => result["patch_error"] = json!(e.to_string()),
            }
            layeragent_tools::git::worktree_remove(repo, &workspace).await;
        }
        let session = handle.session_id.lock().ok().and_then(|s| s.clone());
        let _ = store
            .agent_update(
                &handle.id,
                status,
                session.as_deref(),
                Some(&result.to_string()),
                usage.as_ref(),
            )
            .await;
        if let Ok(mut slot) = handle.result.lock() {
            *slot = Some(result.clone());
        }
        handle.set_status(status);
        // send_replace stores the value even when nobody is waiting yet.
        handle.done.send_replace(true);
        let parent_turn = handle.parent_turn.clone();
        let _ = engine
            .emit(
                &scope,
                &store,
                &parent_session,
                Some(&parent_turn),
                "subagent_completed",
                json!({"agent_id":handle.id,"name":handle.name,"status":status,"error":result["error"]}),
            )
            .await;
        watcher.abort();
        forwarder.abort();
    }
}

/// Surfaces a child's approvals and progress on the parent's event stream, so whoever
/// watches the parent session can answer a child's approval request.
async fn forward_events(
    engine: Arc<Engine>,
    scope: Scope,
    store: Storage,
    handle: Arc<AgentHandle>,
    parent_session: String,
) {
    let mut events = engine.events.subscribe();
    let parent_turn = handle.parent_turn.clone();
    while let Ok((event_scope, event)) = events.recv().await {
        if event_scope != scope {
            continue;
        }
        let own = handle.session_id.lock().ok().and_then(|s| s.clone());
        if own.as_deref() != Some(event.session_id.as_str()) {
            continue;
        }
        let (kind, data) = match event.kind.as_str() {
            "approval_required" => {
                let mut data = event.data.clone();
                data["agent_id"] = json!(handle.id);
                data["agent_name"] = json!(handle.name);
                ("approval_required".to_string(), data)
            }
            "tool_started" | "failed" | "interrupted" | "turn_completed" => (
                "subagent_event".to_string(),
                json!({"agent_id":handle.id,"kind":event.kind,"data":event.data}),
            ),
            _ => continue,
        };
        let _ = engine
            .emit(
                &scope,
                &store,
                &parent_session,
                Some(&parent_turn),
                &kind,
                data,
            )
            .await;
    }
}

impl Engine {
    pub(crate) fn bind_child_session(&self, ctl: &AgentCtl, session: &str) {
        if let Some(id) = &ctl.agent_id
            && let Ok(agents) = ctl.tree.agents.lock()
            && let Some(handle) = agents.get(id)
            && let Ok(mut slot) = handle.session_id.lock()
        {
            *slot = Some(session.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_exhaustion_is_sticky_and_shared() {
        let b = Budget::new(Some(100), None);
        assert!(b.charge(60, 0.0));
        assert!(!b.charge(60, 0.0));
        assert!(b.exhausted());
        // Even a zero-cost charge afterwards cannot revive the tree.
        assert!(!b.charge(0, 0.0));
    }

    #[test]
    fn cost_cap_also_exhausts() {
        let b = Budget::new(None, Some(0.01));
        assert!(b.charge(1_000_000, 0.005));
        assert!(!b.charge(1, 0.006));
    }

    #[test]
    fn children_are_never_wider_than_their_parent() {
        let mut base = Config::default();
        base.policy.write = true;
        base.policy.network = true;
        base.agents.max_depth = 2;
        let base = Arc::new(base);
        let root = AgentCtl::root(
            &base.agents,
            false,
            EffortLevel::Auto,
            RunMode::Normal,
            ExecutionMode::Delegated,
        );
        assert!(Arc::ptr_eq(&effective_config(base.clone(), &root), &base));
        let child = AgentCtl {
            tree: root.tree.clone(),
            depth: 1,
            agent_id: Some("a".into()),
            writable: false,
            shares_workspace: true,
            inherited_effort: Some(EffortLevel::Medium),
            ancestors: vec!["t".into()],
            bound: OnceLock::new(),
            permit: Mutex::new(None),
        };
        let c = effective_config(base.clone(), &child);
        assert!(!c.policy.write && !c.policy.network && !c.auto_skills);
        assert!(c.agents.enabled, "depth 1 of 2 may still delegate");
        let leaf = AgentCtl { depth: 2, ..child };
        assert!(!effective_config(base, &leaf).agents.enabled);
    }
}
