//! Durable task, correction, connection-authority and measurement records.
use crate::Storage;
use anyhow::{Context, Result, ensure};
use layeragent_protocol::task::{CancellationState, CorrectTask, Task, TaskCorrection, TaskState};
use layeragent_protocol::{Scope, new_id, now_ms};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// Typed conflict so HTTP callers receive 409 without parsing message text.
#[derive(Debug, thiserror::Error)]
#[error("idempotency key was already used with different task content")]
pub struct TaskConflict;

/// Stable creation result: replaying an identical request must return the same task.
#[derive(Debug)]
pub enum TaskCreate {
    Created(Task),
    Replayed(Task),
}

fn read_task(c: &Connection, id: &str) -> Result<Task> {
    c.query_row(
        "SELECT id,tenant,principal,project,session_id,turn_id,objective,executor,state,revision,usage,cancellation,correction,error,last_event_seq,effort,mode,bridge,execution,team_id,created_at,updated_at FROM tasks WHERE id=?1",
        [id],
        |r| {
            let state: String = r.get(8)?;
            let cancellation: String = r.get(11)?;
            let effort: String = r.get(15)?;
            let mode: String = r.get(16)?;
            let execution: String = r.get(18)?;
            Ok(Task {
                id: r.get(0)?,
                tenant: r.get(1)?,
                principal: r.get(2)?,
                project: r.get(3)?,
                session_id: r.get(4)?,
                turn_id: r.get(5)?,
                objective: r.get(6)?,
                executor: r.get(7)?,
                state: state.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        8,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(format!("{e}"))),
                    )
                })?,
                revision: r.get(9)?,
                usage: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or(Value::Null),
                cancellation: cancellation.parse().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        11,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::other(format!("{e}"))),
                    )
                })?,
                correction: r.get(12)?,
                error: r.get(13)?,
                last_event_seq: r.get(14)?,
                effort: effort.parse().unwrap_or_default(),
                mode: if mode == "illustration" { layeragent_protocol::RunMode::Illustration } else { layeragent_protocol::RunMode::Normal },
                bridge: r.get(17)?,
                execution: execution.parse().unwrap_or_default(),
                team_id: r.get(19)?,
                created_at: r.get(20)?,
                updated_at: r.get(21)?,
            })
        },
    )
    .context("task not found or inaccessible")
}

fn add_event(c: &Connection, task: &str, kind: &str, data: Value, at: i64) -> Result<()> {
    c.execute(
        "INSERT INTO task_events(task_id,at,kind,data) VALUES(?1,?2,?3,?4)",
        params![task, at, kind, data.to_string()],
    )?;
    Ok(())
}

impl Storage {
    #[allow(clippy::too_many_arguments)]
    pub async fn create_task(
        &self,
        scope: &Scope,
        project: &str,
        session_id: Option<&str>,
        objective: &str,
        executor: &str,
        idempotency_key: &str,
        request_sha256: &str,
    ) -> Result<TaskCreate> {
        self.create_task_with_options(
            scope,
            project,
            session_id,
            objective,
            executor,
            idempotency_key,
            request_sha256,
            Default::default(),
            Default::default(),
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_task_with_options(
        &self,
        scope: &Scope,
        project: &str,
        session_id: Option<&str>,
        objective: &str,
        executor: &str,
        idempotency_key: &str,
        request_sha256: &str,
        effort: layeragent_protocol::EffortLevel,
        mode: layeragent_protocol::RunMode,
        bridge: bool,
    ) -> Result<TaskCreate> {
        self.create_task_with_execution(
            scope,
            project,
            session_id,
            objective,
            executor,
            idempotency_key,
            request_sha256,
            effort,
            mode,
            bridge,
            Default::default(),
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_task_with_execution(
        &self,
        scope: &Scope,
        project: &str,
        session_id: Option<&str>,
        objective: &str,
        executor: &str,
        idempotency_key: &str,
        request_sha256: &str,
        effort: layeragent_protocol::EffortLevel,
        mode: layeragent_protocol::RunMode,
        bridge: bool,
        execution: layeragent_protocol::ExecutionMode,
        cooperative: Option<layeragent_protocol::CooperativeOptions>,
    ) -> Result<TaskCreate> {
        ensure!(
            !idempotency_key.is_empty() && idempotency_key.len() <= 200,
            "idempotency key must contain 1..200 bytes"
        );
        ensure!(
            !objective.trim().is_empty() && objective.len() <= 262_144,
            "objective must contain 1..262144 bytes"
        );
        ensure!(
            (execution == layeragent_protocol::ExecutionMode::Cooperative) == cooperative.is_some(),
            "cooperative options must match execution mode"
        );
        let cooperative = cooperative
            .map(|options| options.normalized(None))
            .transpose()?;
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        let (project, session_id) = (project.to_string(), session_id.map(str::to_string));
        let objective = layeragent_protocol::redact::redact_text(objective);
        let executor = layeragent_protocol::redact::redact_text(executor);
        let (key, fingerprint) = (idempotency_key.to_string(), request_sha256.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            if let Some((id, prior)) = tx
                .query_row(
                    "SELECT id,request_sha256 FROM tasks WHERE tenant=?1 AND principal=?2 AND idempotency_key=?3",
                    params![tenant, principal, key],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
            {
                ensure!(prior == fingerprint, TaskConflict);
                let task = read_task(&tx, &id)?;
                tx.commit()?;
                return Ok(TaskCreate::Replayed(task));
            }
            let id = new_id();
            let at = now_ms();
            let cooperative_body = cooperative.as_ref().map(serde_json::to_string).transpose()?;
            tx.execute(
                "INSERT INTO tasks(id,tenant,principal,project,session_id,turn_id,objective,executor,state,revision,usage,cancellation,last_event_seq,idempotency_key,request_sha256,created_at,updated_at,effort,mode,bridge,execution,cooperative) VALUES(?1,?2,?3,?4,?5,?1,?6,?7,'prepared',1,'{}','none',0,?8,?9,?10,?10,?11,?12,?13,?14,?15)",
                params![id, tenant, principal, project, session_id, objective, executor, key, fingerprint, at, effort.as_str(), mode.as_str(), bridge, execution.as_str(), cooperative_body],
            )?;
            let team_id = if let Some(options) = &cooperative {
                let scope = Scope { tenant: tenant.clone(), principal: principal.clone() };
                let team = crate::team::create_team_in_tx(&tx, &id, &scope, &project, options, at)?;
                tx.execute("UPDATE tasks SET team_id=?2 WHERE id=?1", params![id, team])?;
                Some(team)
            } else { None };
            add_event(
                &tx,
                &id,
                "task_prepared",
                json!({"executor": executor, "objective": objective, "project": project, "execution": execution, "team_id": team_id}),
                at,
            )?;
            let task = read_task(&tx, &id)?;
            tx.commit()?;
            Ok(TaskCreate::Created(task))
        })
        .await
    }

    pub async fn task_binding(&self, turn: &str) -> Result<Option<(i64, i64)>> {
        let turn = turn.to_string();
        self.read(move |c| {
            Ok(c.query_row(
                "SELECT revision,created_at FROM tasks WHERE turn_id=?1",
                [turn],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?)
        })
        .await
    }

    pub async fn task(&self, scope: &Scope, id: &str) -> Result<Task> {
        let (tenant, principal, id) = (
            scope.tenant.clone(),
            scope.principal.clone(),
            id.to_string(),
        );
        self.read(move |c| {
            let task = read_task(c, &id)?;
            ensure!(
                task.tenant == tenant && task.principal == principal,
                "task not found or inaccessible"
            );
            Ok(task)
        })
        .await
    }

    /// Cursor-paged task list. `next` is the last rowid, matching the cursor
    /// comparison, so a client can page without re-reading task metadata.
    pub async fn tasks(
        &self,
        scope: &Scope,
        after: i64,
        limit: usize,
    ) -> Result<(Vec<Task>, Option<i64>)> {
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        let limit = limit.clamp(1, 200) as i64;
        self.read(move |c| {
            let mut q = c.prepare("SELECT rowid,id,tenant,principal,project,session_id,turn_id,objective,executor,state,revision,usage,cancellation,correction,error,last_event_seq,effort,mode,bridge,execution,team_id,created_at,updated_at FROM tasks WHERE tenant=?1 AND principal=?2 AND rowid>?3 ORDER BY rowid LIMIT ?4")?;
            let rows = q
                .query_map(params![tenant, principal, after, limit], |r| {
                    let state: String = r.get(9)?;
                    let cancellation: String = r.get(12)?;
                    let effort: String = r.get(16)?;
                    let mode: String = r.get(17)?;
                    let execution: String = r.get(19)?;
                    Ok((r.get::<_,i64>(0)?, Task {
                        id: r.get(1)?,
                        tenant: r.get(2)?,
                        principal: r.get(3)?,
                        project: r.get(4)?,
                        session_id: r.get(5)?,
                        turn_id: r.get(6)?,
                        objective: r.get(7)?,
                        executor: r.get(8)?,
                        state: state.parse().unwrap_or(TaskState::Blocked),
                        revision: r.get(10)?,
                        usage: serde_json::from_str(&r.get::<_, String>(11)?)
                            .unwrap_or(Value::Null),
                        cancellation: cancellation
                            .parse()
                            .unwrap_or(CancellationState::Uncertain),
                        correction: r.get(13)?,
                        error: r.get(14)?,
                        last_event_seq: r.get(15)?,
                        effort: effort.parse().unwrap_or_default(),
                        mode: if mode == "illustration" { layeragent_protocol::RunMode::Illustration } else { layeragent_protocol::RunMode::Normal },
                        bridge: r.get(18)?,
                        execution: execution.parse().unwrap_or_default(),
                        team_id: r.get(20)?,
                        created_at: r.get(21)?,
                        updated_at: r.get(22)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Only advertise another page when this page was full; a short
            // page is the tail and avoids an extra empty round-trip.
            let next = if rows.len() as i64 == limit {
                rows.last().map(|(rowid, _)| *rowid)
            } else {
                None
            };
            Ok((rows.into_iter().map(|(_, task)| task).collect(), next))
        })
        .await
    }

    pub async fn task_events(&self, id: &str, after: i64, limit: usize) -> Result<Vec<Value>> {
        let id = id.to_string();
        let limit = limit.clamp(1, 1000) as i64;
        self.read(move |c| {
            let mut q = c.prepare(
                "SELECT seq,at,kind,data FROM task_events WHERE task_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3",
            )?;
            let rows = q
                .query_map(params![id, after, limit], |r| {
                    Ok(json!({
                        "seq":r.get::<_,i64>(0)?,
                        "at":r.get::<_,i64>(1)?,
                        "task_id":id,
                        "kind":r.get::<_,String>(2)?,
                        "data":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null)
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Bind the durable task to the turn the engine actually created. Called
    /// before work starts so a crash between task and turn cannot masquerade as
    /// a live task after recovery.
    pub async fn bind_task_turn(&self, task: &str, session: &str, turn: &str) -> Result<()> {
        let (task, session, turn) = (task.to_string(), session.to_string(), turn.to_string());
        self.write(move |c| {
            c.execute(
                "UPDATE tasks SET session_id=?2,turn_id=?3,updated_at=?4,revision=revision+1 WHERE id=?1",
                params![task, session, turn, now_ms()],
            )?;
            Ok(())
        })
        .await
    }

    /// Mirrors engine events into the task log in the same writer transaction as
    /// the session event. A lost client cannot lose task progress.
    pub async fn mirror_task_event_by_turn(
        &self,
        turn: &str,
        kind: &str,
        data: &Value,
    ) -> Result<()> {
        let (turn, kind, data) = (turn.to_string(), kind.to_string(), data.to_string());
        self.write(move |c| {
            c.execute(
                "INSERT INTO task_events(task_id,at,kind,data) SELECT id,?2,?3,?4 FROM tasks WHERE turn_id=?1",
                params![turn, now_ms(), kind, data],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn correct_task(
        &self,
        scope: &Scope,
        task_id: &str,
        request: CorrectTask,
    ) -> Result<TaskCorrection> {
        let (tenant, principal, task_id, request) = (
            scope.tenant.clone(),
            scope.principal.clone(),
            task_id.to_string(),
            request,
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let (revision, state): (i64, String) = tx.query_row(
                "SELECT revision,state FROM tasks WHERE id=?1 AND tenant=?2 AND principal=?3",
                params![task_id, tenant, principal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let state = state.parse::<TaskState>()?;
            ensure!(!state.terminal(), "task already finished");
            ensure!(
                !matches!(state, TaskState::Blocked),
                "task is blocked; reconcile it before correcting"
            );
            ensure!(
                revision == request.expected_revision,
                "task revision changed; reload before correcting"
            );
            ensure!(
                !request.instruction.trim().is_empty() && request.instruction.len() <= 65_536,
                "correction must contain 1..65536 bytes"
            );
            let correction = TaskCorrection {
                id: new_id(),
                task_id: task_id.clone(),
                instruction: layeragent_protocol::redact::redact_text(&request.instruction),
                expected_revision: revision,
                status: "pending".into(),
                created_at: now_ms(),
                applied_at: None,
            };
            tx.execute(
                "INSERT INTO task_corrections(id,task_id,instruction,expected_revision,status,created_at) VALUES(?1,?2,?3,?4,'pending',?5)",
                params![
                    correction.id,
                    correction.task_id,
                    correction.instruction,
                    correction.expected_revision,
                    correction.created_at
                ],
            )?;
            tx.execute(
                "UPDATE tasks SET correction=?2,updated_at=?3 WHERE id=?1",
                params![task_id, correction.instruction, correction.created_at],
            )?;
            add_event(
                &tx,
                &task_id,
                "task_correction_queued",
                json!({"correction_id": correction.id, "instruction": correction.instruction}),
                correction.created_at,
            )?;
            tx.commit()?;
            Ok(correction)
        })
        .await
    }

    /// Claims the oldest pending correction. The engine calls this only between
    /// tool effects, which is the safe point promised by the API.
    pub async fn next_correction(&self, task_id: &str) -> Result<Option<TaskCorrection>> {
        let task_id = task_id.to_string();
        self.write(move |c| {
            let tx = c.transaction()?;
            let row = tx
                .query_row(
                    "SELECT id,task_id,instruction,expected_revision,status,created_at,applied_at FROM task_corrections WHERE task_id=?1 AND status='pending' ORDER BY created_at LIMIT 1",
                    [&task_id],
                    |r| {
                        Ok(TaskCorrection {
                            id: r.get(0)?,
                            task_id: r.get(1)?,
                            instruction: r.get(2)?,
                            expected_revision: r.get(3)?,
                            status: r.get(4)?,
                            created_at: r.get(5)?,
                            applied_at: r.get(6)?,
                        })
                    },
                )
                .optional()?;
            let Some(mut correction) = row else {
                tx.commit()?;
                return Ok(None);
            };
            let at = now_ms();
            correction.status = "applied".into();
            correction.applied_at = Some(at);
            tx.execute(
                "UPDATE task_corrections SET status='applied',applied_at=?2 WHERE id=?1",
                params![correction.id, at],
            )?;
            add_event(
                &tx,
                &task_id,
                "task_correction_applied",
                json!({"correction_id": correction.id, "instruction": correction.instruction}),
                at,
            )?;
            tx.commit()?;
            Ok(Some(correction))
        })
        .await
    }

    /// Returns false when the task already reached a terminal or reconciled state.
    pub async fn request_cancel(&self, scope: &Scope, task_id: &str) -> Result<bool> {
        let (tenant, principal, task_id) = (
            scope.tenant.clone(),
            scope.principal.clone(),
            task_id.to_string(),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let (state, cancellation): (String, String) = tx.query_row(
                "SELECT state,cancellation FROM tasks WHERE id=?1 AND tenant=?2 AND principal=?3",
                params![task_id, tenant, principal],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let state = state.parse::<TaskState>()?;
            if state.terminal() || matches!(state, TaskState::Blocked) {
                tx.commit()?;
                return Ok(false);
            }
            tx.execute(
                "UPDATE tasks SET cancellation='requested',updated_at=?2,revision=revision+1 WHERE id=?1 AND cancellation='none'",
                params![task_id, now_ms()],
            )?;
            add_event(
                &tx,
                &task_id,
                "task_cancellation_requested",
                json!({"previous": cancellation}),
                now_ms(),
            )?;
            tx.commit()?;
            Ok(true)
        })
        .await
    }

    pub async fn finish_task(
        &self,
        task_id: &str,
        usage: &Value,
        cancellation: CancellationState,
        error: Option<&str>,
    ) -> Result<()> {
        let (task_id, usage, error) = (
            task_id.to_string(),
            usage.to_string(),
            error.map(layeragent_protocol::redact::redact_text),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "UPDATE tasks SET usage=?2,cancellation=?3,error=COALESCE(?4,error),updated_at=?5 WHERE id=?1",
                params![task_id, usage, cancellation.as_str(), error, now_ms()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn set_task_blocked(&self, task_id: &str, reason: &str) -> Result<()> {
        let (task_id, reason) = (
            task_id.to_string(),
            layeragent_protocol::redact::redact_text(reason),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            add_event(
                &tx,
                &task_id,
                "task_blocked",
                json!({"reason": reason}),
                now_ms(),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Recovery is explicit and connection-independent. Work that was running
    /// when the host stopped becomes `blocked`; it is never resumed implicitly.
    pub async fn recover_tasks(&self) -> Result<()> {
        self.write(|c| {
            let at = now_ms();
            let mut q = c.prepare("SELECT id,cancellation FROM tasks WHERE state NOT IN ('completed','failed','cancelled')")?;
            let rows = q
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, cancellation) in rows {
                let uncertain = matches!(cancellation.as_str(), "requested" | "uncertain");
                c.execute(
                    "UPDATE tasks SET state='blocked',cancellation=?2,error=?3,updated_at=?4 WHERE id=?1",
                    params![
                        id,
                        if uncertain { "uncertain" } else { "none" },
                        "Runtime restarted; reconcile before retrying",
                        at
                    ],
                )?;
                c.execute(
                    "INSERT INTO task_events(task_id,at,kind,data) VALUES(?1,?2,'task_interrupted',?3)",
                    params![
                        id,
                        at,
                        json!({"reason":"runtime restarted","cancellation_uncertain":uncertain})
                            .to_string()
                    ],
                )?;
            }
            Ok(())
        })
        .await
    }

    pub async fn record_task_metrics(&self, task_id: &str, metrics: &Value) -> Result<()> {
        let (task_id, metrics) = (task_id.to_string(), metrics.clone());
        self.write(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO task_metrics(task_id,model,queue_ms,engine_ms,tool_ms,provider_ms,network_ms,first_content_ms,total_ms,max_rss_bytes,cpu_ms,input_tokens,output_tokens,cost_usd,measured_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                params![
                    task_id,
                    metrics["model"].as_str(),
                    metrics["queue_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["engine_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["tool_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["provider_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["network_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["first_content_ms"].as_u64().map(|v| v as i64),
                    metrics["total_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["max_rss_bytes"].as_u64().unwrap_or(0) as i64,
                    metrics["cpu_ms"].as_u64().unwrap_or(0) as i64,
                    metrics["input_tokens"].as_u64().unwrap_or(0) as i64,
                    metrics["output_tokens"].as_u64().unwrap_or(0) as i64,
                    metrics["cost_usd"].as_f64().unwrap_or(0.0),
                    now_ms()
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// Timing evidence derived from the durable task event log. Provider/network
    /// timings are reported by the router; queue and first-content come from
    /// persisted events, not from the requesting connection.
    pub async fn task_timing(&self, task_id: &str) -> Result<Value> {
        let task_id = task_id.to_string();
        self.read(move |c| {
            let (created_at, updated_at): (i64, i64) = c.query_row(
                "SELECT created_at,updated_at FROM tasks WHERE id=?1",
                [&task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let started = c
                .query_row(
                    "SELECT min(at) FROM task_events WHERE task_id=?1 AND kind='turn_started'",
                    [&task_id],
                    |r| r.get::<_, Option<i64>>(0),
                )?
                .unwrap_or(created_at);
            let first_content = c.query_row(
                "SELECT min(at) FROM task_events WHERE task_id=?1 AND kind='text_delta'",
                [&task_id],
                |r| r.get::<_, Option<i64>>(0),
            )?;
            let network_ms: i64 = c
                .query_row(
                    "SELECT COALESCE(sum(CAST(json_extract(data,'$.ms') AS INTEGER)),0) FROM task_events WHERE task_id=?1 AND kind='network_ms'",
                    [&task_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(json!({
                "created_at": created_at,
                "started_at": started,
                "first_content_at": first_content,
                "finished_at": c.query_row(
                    "SELECT max(at) FROM task_events WHERE task_id=?1 AND kind IN ('turn_completed','failed','interrupted','task_blocked')",
                    [&task_id],
                    |r| r.get::<_, Option<i64>>(0),
                )?.unwrap_or(updated_at),
                "network_ms": network_ms,
            }))
        })
        .await
    }

    pub async fn task_metrics(&self, task_id: &str) -> Result<Option<Value>> {
        let task_id = task_id.to_string();
        self.read(move |c| {
            Ok(c.query_row(
                "SELECT model,queue_ms,engine_ms,tool_ms,provider_ms,network_ms,first_content_ms,total_ms,max_rss_bytes,cpu_ms,input_tokens,output_tokens,cost_usd,measured_at FROM task_metrics WHERE task_id=?1",
                [task_id],
                |r| {
                    Ok(json!({
                        "model":r.get::<_,Option<String>>(0)?,
                        "queue_ms":r.get::<_,i64>(1)?,
                        "engine_ms":r.get::<_,i64>(2)?,
                        "tool_ms":r.get::<_,i64>(3)?,
                        "provider_ms":r.get::<_,i64>(4)?,
                        "network_ms":r.get::<_,i64>(5)?,
                        "first_content_ms":r.get::<_,Option<i64>>(6)?,
                        "total_ms":r.get::<_,i64>(7)?,
                        "max_rss_bytes":r.get::<_,i64>(8)?,
                        "cpu_ms":r.get::<_,i64>(9)?,
                        "input_tokens":r.get::<_,i64>(10)?,
                        "output_tokens":r.get::<_,i64>(11)?,
                        "cost_usd":r.get::<_,f64>(12)?,
                        "measured_at":r.get::<_,i64>(13)?
                    }))
                },
            )
            .optional()?)
        })
        .await
    }

    /// Bind an artifact to the exact project/session/task that produced or
    /// consumed it. Principal-only access remains for legacy local tools.
    pub async fn link_artifact(
        &self,
        scope: &Scope,
        session: Option<&str>,
        task: Option<&str>,
        hash: &str,
    ) -> Result<()> {
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        let (session, task, hash) = (
            session.map(str::to_string),
            task.map(str::to_string),
            hash.to_string(),
        );
        self.write(move |c| {
            c.execute(
                "INSERT OR IGNORE INTO artifact_links(hash,tenant,principal,session_id,task_id,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![hash, tenant, principal, session, task, now_ms()],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn artifact_scoped(
        &self,
        scope: &Scope,
        session: Option<&str>,
        task: Option<&str>,
        hash: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        let (session, task, hash) = (
            session.map(str::to_string),
            task.map(str::to_string),
            hash.to_string(),
        );
        let lookup = hash.clone();
        let allowed = self
            .read(move |c| {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM artifact_links WHERE hash=?1 AND tenant=?2 AND principal=?3 AND (?4 IS NULL OR session_id=?4) AND (?5 IS NULL OR task_id=?5))",
                    params![lookup, tenant, principal, session, task],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await?;
        ensure!(allowed, "artifact not found or inaccessible");
        // Reuse principal-bound access only after the exact scope link passed.
        self.artifact(&scope.principal, &hash, offset, limit).await
    }

    pub async fn connection_state(&self, id: &str) -> Result<Option<(bool, i64)>> {
        let id = id.to_string();
        self.read(move |c| {
            Ok(c.query_row(
                "SELECT enabled,generation FROM connection_states WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, bool>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?)
        })
        .await
    }

    /// A generation observed at the start of an effect must still match when the
    /// effect is about to run. Missing rows are active for configuration defaults.
    pub async fn connection_allows(
        &self,
        id: &str,
        observed_generation: Option<i64>,
    ) -> Result<bool> {
        match self.connection_state(id).await? {
            None => Ok(true),
            Some((enabled, generation)) => {
                Ok(enabled && observed_generation.is_none_or(|observed| observed == generation))
            }
        }
    }

    pub async fn set_connection_enabled(
        &self,
        id: &str,
        kind: &str,
        name: &str,
        enabled: bool,
        probe: Option<&str>,
    ) -> Result<()> {
        let (id, kind, name) = (id.to_string(), kind.to_string(), name.to_string());
        let probe = probe.map(str::to_string);
        self.write(move |c| {
            c.execute(
                "INSERT INTO connection_states(id,kind,name,enabled,generation,updated_at,last_probe) VALUES(?1,?2,?3,?4,1,?5,?6) ON CONFLICT(id) DO UPDATE SET enabled=?4,updated_at=?5,last_probe=COALESCE(?6,last_probe)",
                params![id, kind, name, enabled, now_ms(), probe],
            )?;
            Ok(())
        })
        .await
    }

    /// Revocation invalidates every authority token observed before it and
    /// disables new effects until an operator explicitly re-enables the service.
    pub async fn revoke_connection(&self, id: &str, kind: &str, name: &str) -> Result<i64> {
        let (id, kind, name) = (id.to_string(), kind.to_string(), name.to_string());
        self.write(move |c| {
            c.execute(
                "INSERT INTO connection_states(id,kind,name,enabled,generation,updated_at) VALUES(?1,?2,?3,0,2,?4) ON CONFLICT(id) DO UPDATE SET enabled=0,generation=generation+1,updated_at=?4",
                params![id, kind, name, now_ms()],
            )?;
            c.query_row(
                "SELECT generation FROM connection_states WHERE id=?1",
                [id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .await
    }

    pub async fn active_task_count(&self, scope: &Scope) -> Result<i64> {
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        self.read(move |c| {
            Ok(c.query_row(
                "SELECT count(*) FROM tasks WHERE tenant=?1 AND principal=?2 AND state IN ('prepared','running','awaiting_permission','blocked')",
                params![tenant, principal],
                |r| r.get(0),
            )?)
        })
        .await
    }

    /// Cancels work that never reached the engine (created, not yet bound to a
    /// running turn). The distinct `confirmed` cancellation state is persisted
    /// so a reload cannot mistake it for work still in flight.
    pub async fn cancel_unstarted(&self, task_id: &str, reason: &str) -> Result<()> {
        let (task_id, reason) = (
            task_id.to_string(),
            layeragent_protocol::redact::redact_text(reason),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let state: String =
                tx.query_row("SELECT state FROM tasks WHERE id=?1", [&task_id], |r| {
                    r.get(0)
                })?;
            if matches!(state.as_str(), "completed" | "failed" | "cancelled") {
                tx.commit()?;
                return Ok(());
            }
            tx.execute(
                "UPDATE tasks SET cancellation='confirmed',updated_at=?2 WHERE id=?1",
                params![task_id, now_ms()],
            )?;
            add_event(
                &tx,
                &task_id,
                "task_cancelled",
                json!({"reason": reason, "cancellation": "confirmed"}),
                now_ms(),
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Token and cost totals for one principal. Measurement rows are joined so
    /// estimated cost reflects the recorded per-task estimate, not a client sum.
    pub async fn usage_totals(&self, scope: &Scope) -> Result<Value> {
        let (tenant, principal) = (scope.tenant.clone(), scope.principal.clone());
        self.read(move |c| {
            let (tasks, input, output, usage_cost): (i64, i64, i64, f64) = c.query_row(
                "SELECT count(*),COALESCE(sum(CAST(json_extract(usage,'$.input_tokens') AS INTEGER)),0),COALESCE(sum(CAST(json_extract(usage,'$.output_tokens') AS INTEGER)),0),COALESCE(sum(CAST(json_extract(usage,'$.cost_usd') AS REAL)),0) FROM tasks WHERE tenant=?1 AND principal=?2",
                params![tenant, principal],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let measured_cost: f64 = c
                .query_row(
                    "SELECT COALESCE(sum(m.cost_usd),0) FROM task_metrics m JOIN tasks t ON t.id=m.task_id WHERE t.tenant=?1 AND t.principal=?2",
                    params![tenant, principal],
                    |r| r.get(0),
                )
                .unwrap_or(0.0);
            Ok(json!({
                "tasks": tasks,
                "input_tokens": input,
                "output_tokens": output,
                "cost_usd": if measured_cost > 0.0 { measured_cost } else { usage_cost },
            }))
        })
        .await
    }

    /// Every configured connection state, keyed for the connections view.
    pub async fn all_connection_states(&self) -> Result<Vec<Value>> {
        self.read(|c| {
            let mut q = c.prepare(
                "SELECT id,kind,name,enabled,generation,updated_at,last_probe FROM connection_states ORDER BY id",
            )?;
            let rows = q
                .query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_,String>(0)?,
                        "kind": r.get::<_,String>(1)?,
                        "name": r.get::<_,String>(2)?,
                        "enabled": r.get::<_,bool>(3)?,
                        "generation": r.get::<_,i64>(4)?,
                        "updated_at": r.get::<_,i64>(5)?,
                        "last_probe": r.get::<_,Option<String>>(6)?,
                    }))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }
}
