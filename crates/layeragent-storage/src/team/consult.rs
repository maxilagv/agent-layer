//! Consultants: a member brings in another model for one question.
//!
//! A consultant joins read-only with a token budget and the question as its
//! first delivery. It answers the member that asked and is then retired by
//! the runtime: it never takes work, never receives broadcasts and never
//! occupies a member place. All checks repeat inside the writer transaction.
use super::*;
use layeragent_protocol::{
    CONSULT_DEFAULT_TOKENS, CONSULT_MAX_TOKENS, CONSULT_MIN_TOKENS, MAX_ACTIVE_CONSULTANTS,
    MAX_CONSULT_ACTIVATIONS, MAX_CONSULTATIONS, TeamConsultInput,
};

/// The budget was spent before the consultant's next model call.
#[derive(Debug, thiserror::Error)]
#[error("consultation budget exhausted: {used} of {budget} tokens used")]
pub struct ConsultBudgetExhausted {
    pub used: i64,
    pub budget: i64,
}

/// Tokens a consultant has spent: settled calls at their reported usage,
/// calls whose consumption is unknown at their reserved estimate.
pub(super) fn tokens_used(c: &Connection, member: &str) -> Result<i64> {
    Ok(c.query_row(
        "SELECT COALESCE(sum(CASE WHEN r.state='settled' THEN COALESCE(json_extract(r.actual_usage,'$.input_tokens'),0)+COALESCE(json_extract(r.actual_usage,'$.output_tokens'),0) WHEN r.state='uncertain' THEN r.input_tokens+r.output_tokens ELSE 0 END),0) FROM team_reservations r JOIN team_activations a ON a.id=r.activation_id WHERE a.member_id=?1",
        [member],
        |r| r.get(0),
    )?)
}

/// Refuses a consultant's next model call once its budget is spent. The
/// check uses actual consumption, so the call that crosses the budget
/// completes: a consultation can exceed its budget by at most one call.
pub(super) fn check_budget(c: &Connection, activation: &str) -> Result<()> {
    let row: Option<(String, Option<i64>)> = c
        .query_row(
            "SELECT m.id,m.consult_budget FROM team_activations a JOIN team_members m ON m.id=a.member_id WHERE a.id=?1 AND m.role='consultant'",
            [activation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((member, budget)) = row else {
        return Ok(());
    };
    let budget = budget.unwrap_or(CONSULT_DEFAULT_TOKENS as i64);
    let used = tokens_used(c, &member)?;
    ensure!(used < budget, ConsultBudgetExhausted { used, budget });
    Ok(())
}

/// Decides, when a consultant's activation ends, whether it is done: it
/// answered, failed, spent its budget or ended without answering. A
/// consultant waiting for the reply to its own clarifying question stays,
/// within its activation limit; one interrupted by the runtime stays and is
/// told to continue.
pub(super) fn settle(
    c: &Connection,
    team: &str,
    member: &str,
    activation_state: &str,
    reason: Option<&str>,
    at: i64,
) -> Result<()> {
    let (name, state, activations, asker, question, budget): (
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
    ) = c.query_row(
        "SELECT name,state,activations,consult_asker,consult_question,consult_budget FROM team_members WHERE id=?1 AND team_id=?2",
        params![member, team],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    )?;
    // Team-wide pauses, blocks and endings are not the consultation's end.
    if matches!(state.as_str(), "paused" | "blocked" | "terminated") {
        return Ok(());
    }
    let answered: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_messages WHERE team_id=?1 AND sender_member_id=?2 AND state='accepted' AND ((?3 IS NOT NULL AND reply_to=?3) OR (?4 IS NOT NULL AND recipient_member_id=?4 AND purpose IN ('response','claim','observation'))))",
        params![team, member, question, asker],
        |r| r.get(0),
    )?;
    let used = tokens_used(c, member)?;
    let budget = budget.unwrap_or(CONSULT_DEFAULT_TOKENS as i64);
    let interrupted = matches!(activation_state, "interrupted" | "blocked");
    let outcome = if answered {
        "answered"
    } else if used >= budget {
        "budget_exhausted"
    } else if state == "failed" {
        "failed"
    } else if activations < MAX_CONSULT_ACTIVATIONS && (state == "waiting_response" || interrupted)
    {
        if interrupted {
            runtime_notice(
                c,
                team,
                member,
                "Your consultation was interrupted. Continue from your canonical session and answer the question you were brought in for.",
                at,
            )?;
        }
        return Ok(());
    } else {
        "unanswered"
    };
    let detail = match outcome {
        "answered" => "consultation answered".to_string(),
        "budget_exhausted" => format!("consultation budget exhausted ({used} of {budget} tokens)"),
        "failed" => format!(
            "consultation failed: {}",
            reason.unwrap_or("the activation failed")
        ),
        _ => "consultation ended without an answer".to_string(),
    };
    c.execute(
        "UPDATE team_members SET state='terminated',reason=?2,consult_outcome=?3,updated_at=?4 WHERE id=?1",
        params![member, detail, outcome, at],
    )?;
    c.execute(
        "UPDATE team_deliveries SET state='cancelled',reason='consultant retired' WHERE recipient_member_id=?1 AND team_id=?2 AND state='accepted'",
        params![member, team],
    )?;
    // The question must not stay open forever once nobody can answer it.
    if let Some(question) = &question {
        let replied: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM team_messages WHERE team_id=?1 AND reply_to=?2 AND purpose='response' AND state='accepted')",
            params![team, question],
            |r| r.get(0),
        )?;
        if !replied {
            let resolution = if answered {
                "answered by the consultant in a separate message".to_string()
            } else {
                detail.clone()
            };
            let changed = c.execute(
                "UPDATE team_messages SET resolved_at=?2,resolved_by=?3,resolution_reason=?4 WHERE id=?1 AND resolved_at IS NULL",
                params![question, at, member, resolution],
            )?;
            if changed == 1 {
                c.execute(
                    "UPDATE team_deliveries SET resolved_at=?2,reason=?3 WHERE message_id=?1",
                    params![question, at, resolution],
                )?;
            }
        }
    }
    if !answered && let Some(asker) = &asker {
        let deliverable: bool = c.query_row(
            "SELECT state NOT IN ('terminated','failed') FROM team_members WHERE id=?1",
            [asker],
            |r| r.get(0),
        )?;
        if deliverable {
            runtime_notice(
                c,
                team,
                asker,
                &format!(
                    "Consultant {name} retired without answering your question {}: {detail}.",
                    question.as_deref().unwrap_or("?")
                ),
                at,
            )?;
        }
    }
    bump(c, team, at)?;
    event(
        c,
        team,
        "team_consultant_retired",
        json!({"member_id":member,"outcome":outcome,"reason":detail,"tokens_used":used,"budget_tokens":budget,"activations":activations}),
        at,
    )
}

impl Storage {
    /// Brings a consultant into the team and sends it the question. `model`
    /// is the configured alias the runtime resolved; `None` runs the team's
    /// model.
    pub async fn team_consult(
        &self,
        team: &str,
        asker: &str,
        input: TeamConsultInput,
    ) -> Result<Value> {
        let (team, asker) = (team.to_string(), asker.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            ensure!(
                member_team(&tx, &team, &asker)? != TeamRole::Consultant,
                "a consultant cannot bring in another consultant"
            );
            let actor = format!("member:{asker}");
            let request = serde_json::to_value(&input)?;
            if let Some(prior) = operation_replay(&tx, &team, &actor, "consult", &input.idempotency_key, &request)? {
                tx.commit()?;
                return Ok(prior);
            }
            let opts = options(&tx, &team)?;
            let revision: i64 = tx
                .query_row("SELECT revision FROM teams WHERE id=?1 AND state='active'", [&team], |r| r.get(0))
                .optional()?
                .context("team is not accepting consultations")?;
            ensure!(revision == input.expected_revision, TeamConflict);
            let question = input.question.trim();
            ensure!(!question.is_empty(), "the consultation needs a question");
            let budget = input.max_tokens.unwrap_or(CONSULT_DEFAULT_TOKENS);
            ensure!(
                (CONSULT_MIN_TOKENS..=CONSULT_MAX_TOKENS).contains(&budget),
                "max_tokens must be {CONSULT_MIN_TOKENS}..{CONSULT_MAX_TOKENS}"
            );
            let (active, total): (i64, i64) = tx.query_row(
                "SELECT COALESCE(sum(state NOT IN ('terminated','failed')),0),count(*) FROM team_members WHERE team_id=?1 AND role='consultant'",
                [&team],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(
                (active as usize) < MAX_ACTIVE_CONSULTANTS,
                "{MAX_ACTIVE_CONSULTANTS} consultations are already running; wait for one to answer"
            );
            ensure!(
                (total as usize) < MAX_CONSULTATIONS,
                "this team already used its {MAX_CONSULTATIONS} consultations"
            );
            let mut body = question.to_string();
            if let Some(patch) = &input.patch_id {
                let meta = writers::patch_meta(&tx, &team, patch)?;
                ensure!(
                    meta["kind"] == "submission" && meta["state"] == "submitted",
                    "patch {patch} is not waiting for review"
                );
                body.push_str(&format!(
                    "\n\nPatch to review: {patch} ({}). Read it with team_patch_show and decide with team_patch_review; your decision answers this question.",
                    meta["summary"].as_str().unwrap_or_default()
                ));
            }
            ensure!(
                body.len() <= opts.limits.max_message_bytes,
                "the question exceeds the team message limit of {} bytes",
                opts.limits.max_message_bytes
            );
            let name = match &input.name {
                Some(name) => name.trim().to_string(),
                None => format!("consultant-{}", total + 1),
            };
            ensure!(
                !name.is_empty() && name.len() <= 64,
                "consultant name must contain 1..64 bytes"
            );
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM team_members WHERE team_id=?1 AND name=?2)",
                params![team, name],
                |r| r.get(0),
            )?;
            ensure!(!taken, "a member named {name} already exists");
            let id = new_id();
            let at = now_ms();
            tx.execute(
                "INSERT INTO team_members(id,team_id,name,role,model,state,justification,created_at,updated_at,consult_asker,consult_budget) VALUES(?1,?2,?3,'consultant',?4,'inactive',?5,?6,?6,?7,?8)",
                params![id, team, name, input.model, "consultation: one question, read-only, retired after answering", at, asker, budget as i64],
            )?;
            let sent = send_message(
                &tx,
                &team,
                &actor,
                Some(&asker),
                "member",
                TeamMessageInput {
                    idempotency_key: format!("consult-{id}"),
                    expected_revision: revision,
                    recipient: Some(id.clone()),
                    purpose: MessagePurpose::Question,
                    body,
                    work_id: input.work_id.clone(),
                    finding_id: None,
                    evidence_id: None,
                    reply_to: None,
                    provenance: vec![],
                    urgent: false,
                },
            )?;
            ensure!(
                sent["state"] == "accepted",
                "the question was not accepted: {}",
                sent["reason"].as_str().unwrap_or("rejected")
            );
            let question_id = sent["id"].as_str().context("question id")?.to_string();
            tx.execute(
                "UPDATE team_members SET consult_question=?2 WHERE id=?1",
                params![id, question_id],
            )?;
            if let Some(patch) = &input.patch_id {
                // The consultant's decision on the patch answers the question.
                tx.execute(
                    "UPDATE team_patches SET review_message_id=?2 WHERE id=?1",
                    params![patch, question_id],
                )?;
            }
            let result = json!({"consultant_id":id,"name":name,"model":input.model,"question_id":question_id,"budget_tokens":budget,"patch_id":input.patch_id,"state":"inactive"});
            save_operation(&tx, &team, &actor, "consult", &input.idempotency_key, &request, &result, at)?;
            bump(&tx, &team, at)?;
            event(&tx, &team, "team_consultant_joined", json!({"member_id":id,"name":name,"model":input.model,"asker":asker,"question_id":question_id,"budget_tokens":budget,"patch_id":input.patch_id}), at)?;
            tx.commit()?;
            Ok(result)
        })
        .await
    }
}
