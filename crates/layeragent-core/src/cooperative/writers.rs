//! Runtime side of cooperative writers: worktrees, patches and integration.
//!
//! A member that writes runs its activations in its own linked worktree under
//! the runtime state directory, never in the team workspace. Only an approved
//! patch reaches the team workspace, applied whole by the runtime under the
//! workspace lease, one integration per team at a time.
use super::*;
use layeragent_protocol::TeamAccess;
use layeragent_storage::{PatchCarry, PatchSubmission, ReviewOutcome};
use layeragent_tools::git::team as git_team;
use std::path::PathBuf;

/// Largest diff slice returned by one `team_patch_show` call.
const SHOW_BYTES: usize = 48 * 1024;
/// How long an approval waits for a task that holds the team workspace.
const WORKSPACE_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

type Gate = Arc<tokio::sync::Mutex<()>>;
/// One integration at a time per team, so approvals apply in order.
static INTEGRATIONS: std::sync::LazyLock<
    Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

fn integration_gate(team: &str) -> Result<Gate> {
    let mut gates = INTEGRATIONS
        .lock()
        .map_err(|_| anyhow::anyhow!("integration lock poisoned"))?;
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(team).and_then(std::sync::Weak::upgrade) {
        return Ok(gate);
    }
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    gates.insert(team.to_string(), Arc::downgrade(&gate));
    Ok(gate)
}

/// The runtime-owned directory holding a team's worktrees.
pub(crate) fn team_area(engine: &Engine, team: &str) -> Result<PathBuf> {
    validate_name(team)?;
    Ok(engine.state_home.join("teams").join(team))
}

fn scratch_file(engine: &Engine, team: &str, label: &str) -> Result<PathBuf> {
    let dir = team_area(engine, team)?.join("tmp");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{label}-{}.patch", new_id())))
}

/// Runs `work` with a scratch file that is removed afterwards.
async fn with_scratch<T, F>(
    engine: &Engine,
    team: &str,
    label: &str,
    work: impl FnOnce(PathBuf) -> F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let file = scratch_file(engine, team, label)?;
    let result = work(file.clone()).await;
    let _ = std::fs::remove_file(&file);
    result
}

/// A path as the claim it would become, checked with the patch rules so a
/// reservation that could never be submitted is refused up front.
fn check_claim(path: &str) -> Result<()> {
    let mut path = path.trim();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    git_team::check_patch_path(path.trim_end_matches('/'))
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// Brings a writing member's worktree up to date before its activation:
/// moves it onto the current team workspace when that changed, and applies
/// work handed over to it. Returns the worktree and notices for the member.
pub(crate) async fn prepare_worktree(
    engine: &Engine,
    store: &Storage,
    context: &TeamActivationContext,
    project: &Path,
) -> Result<(PathBuf, Vec<String>)> {
    let area = team_area(engine, &context.team_id)?;
    let worktree = context
        .worktree
        .as_deref()
        .context("write access without a team worktree")?;
    let worktree = git_team::ensure_under(&area, Path::new(worktree))?;
    let mut notices = Vec::new();
    let state = store
        .team_writer_state(&context.team_id, &context.member_id)
        .await?;
    if state.stale
        && let Some(base) = &state.base
    {
        match git_team::sync(project, &worktree, base, &area.join("tmp")).await {
            Ok((base, conflicts)) => {
                store
                    .team_worktree_synced(&context.team_id, &context.member_id, &base)
                    .await?;
                if !conflicts.is_empty() {
                    notices.push(format!("Runtime worktree notice: the team workspace changed and your worktree was moved onto it. Your changes conflict in: {}. Resolve the conflict markers before submitting.", conflicts.join(", ")));
                }
            }
            Err(error) => notices.push(format!(
                "Runtime worktree notice: your worktree could not be moved onto the current team workspace ({}); it still starts from an older workspace and submitting syncs it again.",
                clip(&error.to_string(), 600)
            )),
        }
    }
    for (patch, body) in store
        .team_pending_carries(&context.team_id, &context.member_id)
        .await?
    {
        let applied = with_scratch(engine, &context.team_id, "carry", |file| {
            let worktree = worktree.clone();
            async move {
                std::fs::write(&file, &body)?;
                git_team::receive(&worktree, &file).await
            }
        })
        .await;
        match applied {
            Ok(conflicts) => {
                store
                    .team_carry_received(&context.team_id, &context.member_id, &patch, &conflicts)
                    .await?;
                notices.push(if conflicts.is_empty() {
                    format!("Runtime worktree notice: work handed over to you (patch {patch}) is now in your worktree.")
                } else {
                    format!("Runtime worktree notice: work handed over to you (patch {patch}) is in your worktree with conflict markers in: {}.", conflicts.join(", "))
                });
            }
            Err(error) => notices.push(format!(
                "Runtime worktree notice: handed-over patch {patch} could not be applied to your worktree ({}); read it with team_patch_show and redo the changes.",
                clip(&error.to_string(), 600)
            )),
        }
    }
    Ok((worktree, notices))
}

impl EngineTeamControl {
    /// The team workspace: where the member's canonical session was opened.
    async fn project(&self, ctx: &ToolContext<'_>) -> Result<PathBuf> {
        Ok(PathBuf::from(
            self.runtime
                .store
                .session(ctx.session_id, &ctx.scope.principal)
                .await?
                .workspace,
        ))
    }

    pub(super) async fn access(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let store = &self.runtime.store;
        let context = &self.runtime.context;
        let (team, member) = (&context.team_id, &context.member_id);
        match required(args, "mode")? {
            "read" => {
                let state = store.team_writer_state(team, member).await?;
                let mut result = store.team_release_write(team, member).await?;
                if let (Some(worktree), Some(base)) = (&state.worktree, &state.base) {
                    let worktree = PathBuf::from(worktree);
                    let files = with_scratch(&self.engine, team, "status", |file| async move {
                        git_team::diff(&worktree, base, &file).await
                    })
                    .await
                    .unwrap_or_default();
                    if !files.is_empty() {
                        result["unsubmitted_files"] = json!(files);
                        result["note"] = json!(
                            "your worktree keeps these changes; write again later to submit them, or hand the work off"
                        );
                    }
                }
                if result["applies"] == "next_activation" {
                    result["next"] = json!(
                        "finish this response; your next activation reads the shared team workspace"
                    );
                }
                Ok(result)
            }
            "write" => {
                ensure!(
                    self.engine.config().await.policy.write,
                    "operator policy does not allow writing"
                );
                let work = required(args, "work_id")?;
                let paths: Vec<String> = args["paths"]
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                for path in &paths {
                    check_claim(path)?;
                }
                store
                    .team_write_precheck(team, member, work, &paths)
                    .await?;
                let state = store.team_writer_state(team, member).await?;
                let mut result = if state.worktree.is_some() {
                    store
                        .team_request_write(team, member, work, &paths, None)
                        .await?
                } else {
                    let project = self.project(ctx).await?;
                    let area = team_area(&self.engine, team)?;
                    let dest = area.join(member);
                    // A leftover from an interrupted run is never reused.
                    git_team::remove(&project, &dest).await;
                    let base = git_team::create(&project, &dest, &area.join("tmp")).await?;
                    let granted = store
                        .team_request_write(
                            team,
                            member,
                            work,
                            &paths,
                            Some((dest.to_string_lossy().into_owned(), base)),
                        )
                        .await;
                    if granted.is_err() {
                        git_team::remove(&project, &dest).await;
                    }
                    granted?
                };
                result["next"] = json!(if result["applies"] == "now" {
                    "the paths are reserved; keep working in your worktree"
                } else {
                    "finish this response now; your next activation runs in your own worktree with write tools, and the team workspace changes only when a reviewer integrates your patch"
                });
                Ok(result)
            }
            other => bail!("unknown access mode {other}"),
        }
    }

    pub(super) async fn submit(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let store = &self.runtime.store;
        let context = &self.runtime.context;
        let (team, member) = (&context.team_id, &context.member_id);
        ensure!(
            context.access == TeamAccess::Write,
            "this activation is read-only; ask for write access with team_access first"
        );
        let state = store.team_writer_state(team, member).await?;
        let worktree = PathBuf::from(state.worktree.as_deref().context("no worktree")?);
        let mut base = state.base.clone().context("worktree base missing")?;
        if state.stale {
            let project = self.project(ctx).await?;
            let area = team_area(&self.engine, team)?;
            let (synced, conflicts) =
                git_team::sync(&project, &worktree, &base, &area.join("tmp")).await?;
            store.team_worktree_synced(team, member, &synced).await?;
            base = synced;
            if !conflicts.is_empty() {
                return Ok(
                    json!({"submitted":false,"synced":true,"conflicts":conflicts,"next":"the team workspace changed under your work; resolve the conflict markers in these files, then submit again"}),
                );
            }
        }
        let (files, body) = with_scratch(&self.engine, team, "submit", |file| {
            let (worktree, base) = (worktree.clone(), base.clone());
            async move {
                let files = git_team::diff(&worktree, &base, &file).await?;
                Ok((files, std::fs::read(&file)?))
            }
        })
        .await?;
        ensure!(
            !files.is_empty(),
            "your worktree has no changes relative to the team workspace"
        );
        let refused: Vec<&String> = files
            .iter()
            .filter(|file| git_team::check_patch_path(file).is_err())
            .collect();
        ensure!(
            refused.is_empty(),
            "the patch touches paths a team patch cannot carry (secrets or Git metadata): {}; revert them",
            refused
                .iter()
                .take(10)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        ensure!(
            !git_team::adds_symlink(&body),
            "the patch creates a symbolic link, which a team patch cannot carry; replace it with a regular file"
        );
        ensure!(
            !git_team::conflict_markers(&body),
            "the patch still contains conflict markers; resolve them first"
        );
        let reviewer = args["reviewer"].as_str().map(str::to_string);
        if let Some(reviewer) = &reviewer {
            // Catch a reviewer that could never approve before asking it.
            let snapshot = store.team_snapshot_by_id(team).await?;
            if snapshot.options.cross_model_review
                && let Some(own) = &context.model_identity
                && super::consult::member_identity(ctx.config, &snapshot, reviewer).as_ref()
                    == Some(own)
            {
                bail!(
                    "this team requires cross-model review and {reviewer} runs {own}, the model you run; name a member on another model, or submit without a reviewer and consult one with team_consult and the patch_id"
                );
            }
        }
        store
            .team_submit_patch(
                team,
                member,
                PatchSubmission {
                    work_id: required(args, "work_id")?.to_string(),
                    summary: required(args, "summary")?.to_string(),
                    base,
                    files,
                    body,
                    reviewer,
                },
            )
            .await
    }

    pub(super) async fn show(&self, args: &Value) -> Result<Value> {
        let (meta, body) = self
            .runtime
            .store
            .team_patch(&self.runtime.context.team_id, required(args, "patch_id")?)
            .await?;
        let text = layeragent_protocol::redact::redact_text(&String::from_utf8_lossy(&body));
        let mut start = (args["offset"].as_u64().unwrap_or(0) as usize).min(text.len());
        while !text.is_char_boundary(start) {
            start -= 1;
        }
        let mut end = (start + SHOW_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(
            json!({"patch":meta,"diff":&text[start..end],"offset":start,"next_offset":(end < text.len()).then_some(end),"total_bytes":text.len()}),
        )
    }

    pub(super) async fn review(&self, ctx: &ToolContext<'_>, args: &Value) -> Result<Value> {
        let store = &self.runtime.store;
        let context = &self.runtime.context;
        let (team, reviewer) = (&context.team_id, &context.member_id);
        let patch = required(args, "patch_id")?;
        let reason = required(args, "reason")?;
        match required(args, "verdict")? {
            "reject" => {
                store
                    .team_review_precheck(team, reviewer, patch, false)
                    .await?;
                store
                    .team_review_record(team, reviewer, patch, ReviewOutcome::Rejected, reason)
                    .await
            }
            "approve" => {
                let gate = integration_gate(team)?;
                let _order = gate.lock().await;
                let (_, body) = store
                    .team_review_precheck(team, reviewer, patch, true)
                    .await?;
                let project = self.project(ctx).await?;
                // Never apply under a task that is writing the same workspace.
                let lease = self.engine.lease(&project.to_string_lossy())?;
                let _workspace = tokio::select! {
                    _ = ctx.cancel.cancelled() => bail!("review cancelled"),
                    guard = tokio::time::timeout(WORKSPACE_WAIT, lease.lock_owned()) => guard.map_err(|_| anyhow::anyhow!("the team workspace is busy with another task; nothing was applied, try again shortly"))?,
                };
                let outcome = with_scratch(&self.engine, team, "integrate", |file| {
                    let project = project.clone();
                    async move {
                        std::fs::write(&file, &body)?;
                        if let Err(output) = git_team::check(&project, &file).await? {
                            return Ok(Err(output));
                        }
                        Ok(git_team::integrate(&project, &file)
                            .await
                            .map_err(|error| error.to_string()))
                    }
                })
                .await?;
                match outcome {
                    Ok(()) => {
                        let recorded = store
                            .team_review_record(
                                team,
                                reviewer,
                                patch,
                                ReviewOutcome::Integrated,
                                reason,
                            )
                            .await;
                        if let Err(error) = &recorded {
                            tracing::error!(%error, patch, "patch applied to the team workspace but its review was not recorded");
                        }
                        let mut result = recorded?;
                        result["applied_to"] = json!("team workspace (working tree, uncommitted)");
                        Ok(result)
                    }
                    Err(output) => {
                        let mut result = store
                            .team_review_record(
                                team,
                                reviewer,
                                patch,
                                ReviewOutcome::Conflicted,
                                &format!(
                                    "{reason} — it no longer applies: {}",
                                    clip(output.trim(), 1200)
                                ),
                            )
                            .await?;
                        result["applied"] = json!(false);
                        Ok(result)
                    }
                }
            }
            other => bail!("unknown verdict {other}"),
        }
    }

    /// A handoff from a writing member also carries its unsubmitted changes to
    /// the reserved paths of that work, which then leave the sender's worktree.
    pub(super) async fn handoff(&self, args: &Value) -> Result<Value> {
        let store = &self.runtime.store;
        let context = &self.runtime.context;
        let (team, member) = (&context.team_id, &context.member_id);
        let input: TeamMessageInput = serde_json::from_value(args.clone())?;
        let state = store.team_writer_state(team, member).await?;
        let work = input.work_id.clone().unwrap_or_default();
        let claims: Vec<String> = state
            .claims
            .iter()
            .filter(|(_, w)| w == &work)
            .map(|(p, _)| p.clone())
            .collect();
        let mut carry = None;
        if let (Some(worktree), Some(base)) = (&state.worktree, &state.base)
            && !claims.is_empty()
        {
            let worktree = PathBuf::from(worktree);
            carry = with_scratch(&self.engine, team, "handoff", |file| {
                let (worktree, base, claims) = (worktree.clone(), base.clone(), claims.clone());
                async move {
                    let files: Vec<String> = git_team::diff(&worktree, &base, &file)
                        .await?
                        .into_iter()
                        .filter(|file| {
                            claims.iter().any(|claim| {
                                file == claim
                                    || file
                                        .strip_prefix(claim.as_str())
                                        .is_some_and(|rest| rest.starts_with('/'))
                            })
                        })
                        .collect();
                    if files.is_empty() {
                        return Ok(None);
                    }
                    git_team::diff_paths(&worktree, &base, &files, &file).await?;
                    Ok(Some(PatchCarry {
                        base,
                        files,
                        body: std::fs::read(&file)?,
                    }))
                }
            })
            .await?;
        }
        let result = store
            .team_member_handoff(team, member, input, carry.clone())
            .await?;
        if result.get("carried_patch").is_some()
            && let (Some(carry), Some(worktree), Some(base)) = (carry, &state.worktree, &state.base)
            && let Err(error) = git_team::revert(Path::new(worktree), base, &carry.files).await
        {
            // The paths are no longer reserved for the sender, so these copies
            // can never be submitted; they only linger in its worktree.
            tracing::warn!(%error, "handed-over changes remain in the sender's worktree");
        }
        Ok(result)
    }
}

impl Engine {
    /// Removes the worktrees of a team that has ended, keeping each member's
    /// unsubmitted changes as a retained patch.
    pub(crate) async fn retire_team_worktrees(&self, store: &Storage, team: &str, project: &Path) {
        let worktrees = match store.team_worktrees(team).await {
            Ok(worktrees) => worktrees,
            Err(error) => {
                tracing::warn!(%error, team, "team worktrees could not be listed");
                return;
            }
        };
        for (member, worktree, base) in worktrees {
            let path = PathBuf::from(&worktree);
            let mut retained = None;
            let mut base = base;
            // A worktree behind the team workspace still holds changes that
            // were already integrated; bring it up to date first so only work
            // that never reached the team workspace is kept.
            let stale = store
                .team_writer_state(team, &member)
                .await
                .is_ok_and(|state| state.stale);
            if stale
                && path.exists()
                && let Some(old) = base.clone()
                && let Ok(area) = team_area(self, team)
            {
                match git_team::sync(project, &path, &old, &area.join("tmp")).await {
                    Ok((synced, _)) => base = Some(synced),
                    Err(error) => {
                        tracing::warn!(%error, team, member, "stale worktree could not be synced before retiring; its changes are kept against the old base");
                    }
                }
            }
            if let Some(base) = base
                && path.exists()
            {
                let captured = async {
                    let file = scratch_file(self, team, "retain")?;
                    let result = async {
                        let files = git_team::diff(&path, &base, &file).await?;
                        Ok::<_, anyhow::Error>(PatchCarry {
                            base: base.clone(),
                            files,
                            body: std::fs::read(&file)?,
                        })
                    }
                    .await;
                    let _ = std::fs::remove_file(&file);
                    result
                }
                .await;
                match captured {
                    Ok(carry) => retained = Some(carry),
                    Err(error) => {
                        // Without a capture the worktree is kept for the operator.
                        tracing::warn!(%error, team, member, "unsubmitted team work could not be captured; worktree kept");
                        continue;
                    }
                }
            }
            if let Err(error) = store.team_retire_worktree(team, &member, retained).await {
                tracing::warn!(%error, team, member, "team worktree could not be retired");
                continue;
            }
            git_team::remove(project, &path).await;
        }
        if let Ok(area) = team_area(self, team) {
            let _ = std::fs::remove_dir_all(area.join("tmp"));
            let _ = std::fs::remove_dir(&area);
        }
    }
}
