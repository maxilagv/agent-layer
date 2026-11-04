//! The usage ledger: every provider call's tokens and estimated cost, so a
//! task's spending can be broken down by model, agent depth and cache use.
use crate::Storage;
use anyhow::Result;
use layeragent_protocol::{Scope, Usage, now_ms};
use rusqlite::params;
use serde_json::{Value, json};

/// One provider call, attributed to the agent tree it ran in.
pub struct UsageEntry<'a> {
    /// The turn the tree belongs to: the task, or a team activation.
    pub root_turn: &'a str,
    pub turn: &'a str,
    pub session: &'a str,
    /// 0 for the turn that started the tree, 1 for its children, and so on.
    pub depth: usize,
    pub model: &'a str,
    pub usage: &'a Usage,
    pub cost_usd: f64,
}

/// A call whose output stays under this many tokens counts as short: many of
/// them on an expensive model usually mean bulk reading or searching.
const SHORT_OUTPUT: i64 = 300;

impl Storage {
    pub async fn record_usage(&self, entry: UsageEntry<'_>) -> Result<()> {
        let row = (
            entry.root_turn.to_string(),
            entry.turn.to_string(),
            entry.session.to_string(),
            entry.depth as i64,
            entry.model.to_string(),
            entry.usage.clone(),
            entry.cost_usd,
        );
        self.write(move |c| {
            let (root, turn, session, depth, model, usage, cost) = row;
            c.execute(
                "INSERT INTO usage_ledger(root_turn_id,turn_id,session_id,depth,model,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,cost_usd,at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    root,
                    turn,
                    session,
                    depth,
                    model,
                    usage.input_tokens as i64,
                    usage.output_tokens as i64,
                    usage.cache_read_tokens as i64,
                    usage.cache_write_tokens as i64,
                    cost,
                    now_ms()
                ],
            )?;
            Ok(())
        })
        .await
    }

    /// What a task spent: in total, per model and per agent depth. A
    /// cooperative task includes every member activation.
    pub async fn usage_report(&self, scope: &Scope, task: &str) -> Result<Value> {
        let budget = self.task(scope, task).await?.budget_usd;
        let task = task.to_string();
        self.read(move |c| {
            let roots = "root_turn_id=?1 OR root_turn_id IN (SELECT a.id FROM team_activations a JOIN teams t ON t.id=a.team_id WHERE t.task_id=?1)";
            let sums = "count(*),COALESCE(sum(input_tokens),0),COALESCE(sum(output_tokens),0),COALESCE(sum(cache_read_tokens),0),COALESCE(sum(cache_write_tokens),0),COALESCE(sum(cost_usd),0.0)";
            let totals = |r: &rusqlite::Row<'_>, offset: usize| -> rusqlite::Result<Value> {
                let input: i64 = r.get(offset + 1)?;
                let read: i64 = r.get(offset + 3)?;
                Ok(json!({
                    "calls": r.get::<_, i64>(offset)?,
                    "input_tokens": input,
                    "output_tokens": r.get::<_, i64>(offset + 2)?,
                    "cache_read_tokens": read,
                    "cache_write_tokens": r.get::<_, i64>(offset + 4)?,
                    "cost_usd": r.get::<_, f64>(offset + 5)?,
                    "cache_read_ratio": if input > 0 { read as f64 / input as f64 } else { 0.0 },
                }))
            };
            let total = c.query_row(
                &format!("SELECT {sums},count(DISTINCT turn_id) FROM usage_ledger WHERE {roots}"),
                [&task],
                |r| {
                    let mut total = totals(r, 0)?;
                    total["turns"] = json!(r.get::<_, i64>(6)?);
                    Ok(total)
                },
            )?;
            let mut q = c.prepare(&format!(
                "SELECT model,{sums},COALESCE(sum(CASE WHEN output_tokens<{SHORT_OUTPUT} THEN 1 ELSE 0 END),0) FROM usage_ledger WHERE {roots} GROUP BY model ORDER BY sum(cost_usd) DESC,model"
            ))?;
            let by_model = q
                .query_map([&task], |r| {
                    let mut row = totals(r, 1)?;
                    row["model"] = json!(r.get::<_, String>(0)?);
                    row["short_calls"] = json!(r.get::<_, i64>(7)?);
                    Ok(row)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut q = c.prepare(&format!(
                "SELECT depth,{sums} FROM usage_ledger WHERE {roots} GROUP BY depth ORDER BY depth"
            ))?;
            let by_depth = q
                .query_map([&task], |r| {
                    let mut row = totals(r, 1)?;
                    row["depth"] = json!(r.get::<_, i64>(0)?);
                    Ok(row)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({
                "task_id": task,
                "budget_usd": budget,
                "total": total,
                "by_model": by_model,
                "by_depth": by_depth,
                "estimate": "costs are estimates from the configured prices, not provider invoices",
            }))
        })
        .await
    }
}
