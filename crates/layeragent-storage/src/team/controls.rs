//! Operator controls on one member: pause it, resume it, or retire it from
//! the team, while the rest of the team carries on.
//!
//! A paused member ends its activation at the next safe boundary instead of
//! holding a concurrency slot, and is not activated again until the operator
//! resumes it; a team resume does not release it. A retired member leaves the
//! team for good: its live activation is interrupted, its work goes back to
//! the coordinator, its reservations and open patches are released, and the
//! members waiting on it are told. The runtime then stops its in-flight call
//! and keeps its unsubmitted worktree changes as a retained patch.
use super::*;

/// What the operator did to one member, as the runtime reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatorHold {
    Paused,
    Retired,
}

impl Storage {
    /// `pause`, `resume` or `retire` on `command.member`.
    pub async fn team_member_command(
        &self,
        scope: &Scope,
        task: &str,
        action: &str,
        command: TeamCommand,
    ) -> Result<Value> {
        ensure!(
            matches!(action, "pause" | "resume" | "retire"),
            "only pause, resume and retire apply to one member"
        );
        ensure!(
            command.recipient.is_none() && command.priority.is_none(),
            "a member command takes member, not recipient or priority"
        );
        let member = command
            .member
            .clone()
            .filter(|member| !member.is_empty())
            .context("member required")?;
        let (scope, task, action) = (scope.clone(), task.to_string(), action.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            let team = scoped_team(&tx, &scope, &task)?;
            let actor = format!("user:{}", scope.principal);
            let kind = format!("member_{action}");
            let request = serde_json::to_value(&command)?;
            if let Some(prior) =
                operation_replay(&tx, &team, &actor, &kind, &command.idempotency_key, &request)?
            {
                tx.commit()?;
                return Ok(prior);
            }
            let (state, revision): (String, i64) = tx.query_row(
                "SELECT state,revision FROM teams WHERE id=?1",
                [&team],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(revision == command.expected_revision, TeamConflict);
            ensure!(
                matches!(state.as_str(), "active" | "paused" | "blocked"),
                "the team is {state}; member controls apply to a running, paused or blocked team"
            );
            let (name, role, member_state, hold): (String, String, String, Option<String>) = tx
                .query_row(
                    "SELECT name,role,state,operator_hold FROM team_members WHERE id=?1 AND team_id=?2",
                    params![member, team],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?
                .context("member not found in this team")?;
            ensure!(
                !matches!(member_state.as_str(), "terminated" | "failed"),
                "member {name} has already left the team"
            );
            let at = now_ms();
            let reason = command.reason.clone().filter(|r| !r.trim().is_empty());
            let live: Option<String> = tx
                .query_row(
                    "SELECT id FROM team_activations WHERE member_id=?1 AND state IN ('starting','running')",
                    [&member],
                    |r| r.get(0),
                )
                .optional()?;
            let mut result = json!({"team_id":team,"member_id":member,"action":action,"revision":revision+1});
            match action.as_str() {
                "pause" => {
                    ensure!(hold.is_none(), "member {name} is already paused by the operator");
                    let detail = match &reason {
                        Some(reason) => format!("paused by the operator: {reason}"),
                        None => "paused by the operator".to_string(),
                    };
                    tx.execute(
                        "UPDATE team_members SET state='paused',reason=?2,operator_hold='paused',operator_hold_at=?3,operator_hold_by=?4,updated_at=?3 WHERE id=?1",
                        params![member, detail, at, actor],
                    )?;
                    result["member_state"] = json!("paused");
                    result["applies"] = json!(if live.is_some() { "next_boundary" } else { "now" });
                }
                "resume" => {
                    ensure!(
                        hold.as_deref() == Some("paused"),
                        "member {name} is not paused by the operator; resume the team instead"
                    );
                    // A paused or blocked team keeps the member waiting with it.
                    let next = if state == "active" {
                        if live.is_some() { "working" } else { "inactive" }
                    } else {
                        member_state.as_str()
                    };
                    tx.execute(
                        "UPDATE team_members SET state=?2,reason=NULL,operator_hold=NULL,operator_hold_at=NULL,operator_hold_by=NULL,updated_at=?3 WHERE id=?1",
                        params![member, next, at],
                    )?;
                    // The notice is a delivery, so the member is activated again
                    // even when its work is already in progress.
                    runtime_notice(
                        &tx,
                        &team,
                        &member,
                        "The operator resumed you after a pause. Continue your work from your canonical session; messages that arrived meanwhile follow.",
                        at,
                    )?;
                    result["member_state"] = json!(next);
                }
                _ => {
                    ensure!(
                        role != "coordinator",
                        "the coordinator cannot be retired; close or cancel the team instead"
                    );
                    let detail = match &reason {
                        Some(reason) => format!("retired by the operator: {reason}"),
                        None => "retired by the operator".to_string(),
                    };
                    let retired = retire(&tx, &team, &member, &name, &role, live.as_deref(), &detail, &actor, at)?;
                    result["member_state"] = json!("terminated");
                    result["retired"] = retired;
                }
            }
            save_operation(&tx, &team, &actor, &kind, &command.idempotency_key, &request, &result, at)?;
            bump(&tx, &team, at)?;
            event(
                &tx,
                &team,
                &format!("team_member_{}", match action.as_str() { "pause" => "paused", "resume" => "resumed", _ => "retired" }),
                json!({"member_id":member,"author":actor,"reason":reason}),
                at,
            )?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// Whether the operator paused or retired this member alone.
    pub async fn team_operator_hold(
        &self,
        team: &str,
        member: &str,
    ) -> Result<Option<OperatorHold>> {
        let (team, member) = (team.to_string(), member.to_string());
        self.read(move |c| {
            let hold: Option<String> = c
                .query_row(
                    "SELECT operator_hold FROM team_members WHERE id=?1 AND team_id=?2",
                    params![member, team],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            Ok(match hold.as_deref() {
                Some("paused") => Some(OperatorHold::Paused),
                Some("retired") => Some(OperatorHold::Retired),
                _ => None,
            })
        })
        .await
    }
}

/// Removes a member from the team in one transaction. Returns what was
/// released, for the operator.
#[allow(clippy::too_many_arguments)]
fn retire(
    c: &Connection,
    team: &str,
    member: &str,
    name: &str,
    role: &str,
    live: Option<&str>,
    detail: &str,
    actor: &str,
    at: i64,
) -> Result<Value> {
    c.execute(
        "UPDATE team_members SET state='terminated',reason=?2,operator_hold='retired',operator_hold_at=?3,operator_hold_by=?4,work_state='idle',consult_outcome=CASE WHEN role='consultant' AND consult_outcome IS NULL THEN 'retired' ELSE consult_outcome END,updated_at=?3 WHERE id=?1",
        params![member, detail, at, actor],
    )?;
    // The activation is fenced at once: its next tool call or provider
    // admission fails, and what it had reserved stays charged.
    if let Some(activation) = live {
        c.execute(
            "UPDATE team_activations SET state='interrupted',reason=?2,finished_at=?3,lease_expires_at=0 WHERE id=?1",
            params![activation, detail, at],
        )?;
        c.execute(
            "UPDATE team_reservations SET state='uncertain',reason='activation ended before provider consumption was settled' WHERE activation_id=?1 AND state='reserved'",
            [activation],
        )?;
        settle_active_time(c, team, at)?;
        event(
            c,
            team,
            "team_activation_finished",
            json!({"activation_id":activation,"member_id":member,"state":"interrupted","reason":detail}),
            at,
        )?;
    }
    c.execute(
        "UPDATE team_deliveries SET state='cancelled',reason='recipient retired by the operator' WHERE recipient_member_id=?1 AND team_id=?2 AND state='accepted'",
        params![member, team],
    )?;
    // Its work goes back to the coordinator, who reassigns or disposes of it.
    let coordinator: Option<String> = c
        .query_row(
            "SELECT id FROM team_members WHERE team_id=?1 AND role='coordinator' AND state NOT IN ('terminated','failed')",
            [team],
            |r| r.get(0),
        )
        .optional()?;
    let work: Vec<String> = {
        let mut q = c.prepare("SELECT id FROM team_work WHERE team_id=?1 AND assignee=?2 AND state IN ('pending','assigned','in_progress') ORDER BY created_at,rowid")?;
        q.query_map(params![team, member], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &work {
        c.execute(
            "UPDATE team_work SET assignee=?2,state='pending',revision=revision+1,updated_at=?3 WHERE id=?1",
            params![id, coordinator, at],
        )?;
    }
    c.execute(
        "DELETE FROM team_path_claims WHERE team_id=?1 AND member_id=?2",
        params![team, member],
    )?;
    let withdrawn: Vec<String> = {
        let mut q = c.prepare("SELECT id FROM team_patches WHERE team_id=?1 AND author_member_id=?2 AND kind='submission' AND state IN ('submitted','rejected','conflicted') ORDER BY created_at,rowid")?;
        q.query_map(params![team, member], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    c.execute(
        "UPDATE team_patches SET state='withdrawn',reason='author retired by the operator',updated_at=?3 WHERE team_id=?1 AND author_member_id=?2 AND kind='submission' AND state IN ('submitted','rejected','conflicted')",
        params![team, member, at],
    )?;
    // Changes handed to it are kept for the operator, not lost with it.
    c.execute(
        "UPDATE team_patches SET state='retained',reason='recipient retired by the operator before applying it',updated_at=?3 WHERE team_id=?1 AND recipient_member_id=?2 AND kind='handoff' AND state='pending'",
        params![team, member, at],
    )?;
    let resolve = |id: &str, why: &str| -> Result<()> {
        c.execute(
            "UPDATE team_messages SET resolved_at=?2,resolved_by=?3,resolution_reason=?4 WHERE id=?1 AND resolved_at IS NULL",
            params![id, at, member, why],
        )?;
        c.execute(
            "UPDATE team_deliveries SET resolved_at=?2,reason=?3 WHERE message_id=?1 AND resolved_at IS NULL",
            params![id, at, why],
        )?;
        Ok(())
    };
    // Its own open questions no longer have anyone to read the answer.
    let asked: Vec<String> = {
        let mut q = c.prepare("SELECT id FROM team_messages WHERE team_id=?1 AND sender_member_id=?2 AND purpose IN ('question','verification_request') AND resolved_at IS NULL")?;
        q.query_map(params![team, member], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for id in &asked {
        resolve(id, "the asking member was retired by the operator")?;
    }
    // Members waiting on it are told instead of waiting forever.
    let waiting: Vec<(String, String)> = {
        let mut q = c.prepare("SELECT m.id,m.sender_member_id FROM team_messages m JOIN team_members s ON s.id=m.sender_member_id WHERE m.team_id=?1 AND m.recipient_member_id=?2 AND m.purpose IN ('question','verification_request') AND m.resolved_at IS NULL AND m.state='accepted' AND s.state NOT IN ('terminated','failed') AND NOT EXISTS(SELECT 1 FROM team_messages r WHERE r.team_id=m.team_id AND r.reply_to=m.id AND r.purpose='response' AND r.state='accepted')")?;
        q.query_map(params![team, member], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (id, sender) in &waiting {
        resolve(id, "the recipient was retired by the operator")?;
        runtime_notice(
            c,
            team,
            sender,
            &format!(
                "The operator retired {name} ({detail}); it will not answer your message {id}. Ask another member if you still need the answer."
            ),
            at,
        )?;
    }
    if let Some(coordinator) = &coordinator
        && (!work.is_empty() || !withdrawn.is_empty())
    {
        let mut body = format!("The operator retired {role} {name} ({detail}).");
        if !work.is_empty() {
            body.push_str(&format!(
                " Its work {} is pending for you again: claim it, hand it to another member or dispose of it.",
                work.join(", ")
            ));
        }
        if !withdrawn.is_empty() {
            body.push_str(&format!(
                " Its patches {} were withdrawn and stay listed for the operator.",
                withdrawn.join(", ")
            ));
        }
        runtime_notice(c, team, coordinator, &body, at)?;
    }
    Ok(
        json!({"interrupted_activation":live,"work_returned":work,"patches_withdrawn":withdrawn,"questions_resolved":asked.len()+waiting.len()}),
    )
}
