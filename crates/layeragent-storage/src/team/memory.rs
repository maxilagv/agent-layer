//! Project memory: what earlier teams on the same workspace concluded.
//!
//! Memory is private to its owner: only teams of the same tenant and
//! principal on the same workspace path see each other's conclusions, and
//! only once the earlier team has ended. Each conclusion keeps the snapshot
//! it was valid for, so a member can tell whether the code changed since.
use super::*;

/// Finding states that end an investigation one way or another.
const CONCLUDED: [&str; 4] = ["discarded", "inconclusive", "reviewed", "reproduced"];
/// Longest evidence output carried into a memory entry.
const OBSERVED_BYTES: usize = 600;

/// Which earlier conclusions to return.
#[derive(Clone, Debug, Default)]
pub struct MemoryQuery {
    /// One of the concluded finding states.
    pub state: Option<String>,
    /// Case-insensitive text the claim or reason contains.
    pub text: Option<String>,
    pub limit: usize,
}

impl Storage {
    /// Earlier conclusions for a running team, as its members see them.
    pub async fn team_project_memory(&self, team: &str, query: MemoryQuery) -> Result<Vec<Value>> {
        let team = team.to_string();
        self.read(move |c| project_memory(c, &team, &query)).await
    }

    /// Earlier conclusions for the team of `task`, for the operator.
    pub async fn team_project_memory_scoped(
        &self,
        scope: &Scope,
        task: &str,
        query: MemoryQuery,
    ) -> Result<Vec<Value>> {
        let (scope, task) = (scope.clone(), task.to_string());
        self.read(move |c| {
            let team = scoped_team(c, &scope, &task)?;
            project_memory(c, &team, &query)
        })
        .await
    }
}

fn shorten(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.to_string();
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

pub(super) fn project_memory(
    c: &Connection,
    team: &str,
    query: &MemoryQuery,
) -> Result<Vec<Value>> {
    if let Some(state) = &query.state {
        ensure!(
            CONCLUDED.contains(&state.as_str()),
            "state must be one of {}",
            CONCLUDED.join(", ")
        );
    }
    let text = query
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_lowercase);
    if let Some(text) = &text {
        ensure!(text.len() <= 200, "query must be at most 200 bytes");
    }
    let limit = query.limit.clamp(1, PROJECT_MEMORY_MAX_ITEMS) as i64;
    let current: Option<String> = c.query_row(
        "SELECT snapshot_sha256 FROM teams WHERE id=?1",
        [team],
        |r| r.get(0),
    )?;
    let mut q = c.prepare(
        "SELECT f.id,f.claim,f.state,f.reason,f.valid_for_snapshot,f.updated_at,t.task_id,tk.objective FROM team_findings f JOIN teams t ON t.id=f.team_id JOIN teams cur ON cur.id=?1 LEFT JOIN tasks tk ON tk.id=t.task_id WHERE t.tenant=cur.tenant AND t.principal=cur.principal AND t.workspace=cur.workspace AND t.id<>cur.id AND t.state IN ('terminated','cancelled','failed') AND f.state IN ('discarded','inconclusive','reviewed','reproduced') AND (?2 IS NULL OR f.state=?2) AND (?3 IS NULL OR instr(lower(f.claim||' '||f.reason),?3)>0) ORDER BY f.updated_at DESC,f.rowid DESC LIMIT ?4",
    )?;
    let rows = q
        .query_map(params![team, query.state, text, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    let mut items = Vec::with_capacity(rows.len());
    for (id, claim, state, reason, snapshot, at, task, objective) in rows {
        let evidence: Option<Value> = c
            .query_row(
                "SELECT id,tool,invocation,exit_code,expected,observed,error,snapshot_sha256 FROM team_evidence WHERE finding_id=?1 ORDER BY created_at DESC,rowid DESC LIMIT 1",
                [&id],
                |r| {
                    Ok(json!({
                        "id":r.get::<_,String>(0)?,
                        "tool":r.get::<_,String>(1)?,
                        "invocation":r.get::<_,String>(2)?,
                        "exit_code":r.get::<_,Option<i64>>(3)?,
                        "expected":r.get::<_,Option<String>>(4)?,
                        "observed":r.get::<_,Option<String>>(5)?.map(|o| shorten(&o, OBSERVED_BYTES)),
                        "error":r.get::<_,Option<String>>(6)?,
                        "snapshot_sha256":r.get::<_,String>(7)?,
                    }))
                },
            )
            .optional()?;
        let same = match (&snapshot, &current) {
            (Some(then), Some(now)) => json!(then == now),
            _ => Value::Null,
        };
        let mut item = json!({
            "finding_id":id,"state":state,"claim":claim,"reason":reason,
            "valid_for_snapshot":snapshot,"same_snapshot":same,"concluded_at":at,
            "task_id":task,"objective":objective.map(|o| shorten(&o, 200)),"evidence":evidence,
        });
        // Model text and tool output cross into another team here.
        layeragent_protocol::redact::redact_value(&mut item);
        items.push(item);
    }
    Ok(items)
}

/// The dead-ends document of an export, from its already redacted rows.
pub(super) fn dead_ends_markdown(export: &TeamExport, earlier: &[Value]) -> String {
    let mut out = format!(
        "# Dead ends: team {}\n\nWhat this team ruled out or could not settle, with the evidence it relied on. A dead end holds for the snapshot it was recorded on; recheck it if the code changed.\n",
        export.team.task_id
    );
    let ended: Vec<&Value> = export
        .findings
        .iter()
        .filter(|f| matches!(f["state"].as_str(), Some("discarded" | "inconclusive")))
        .collect();
    out.push_str("\n## Findings\n");
    if ended.is_empty() {
        out.push_str("\nNo finding was discarded or left inconclusive.\n");
    }
    for finding in ended {
        let id = finding["id"].as_str().unwrap_or("");
        out.push_str(&format!(
            "\n### {} — {}\n\n- Finding `{}`, snapshot `{}` ({})\n- Why: {}\n",
            finding["state"].as_str().unwrap_or(""),
            inline(&finding["claim"]),
            id,
            finding["valid_for_snapshot"].as_str().unwrap_or("unknown"),
            finding["validity"].as_str().unwrap_or("unknown"),
            inline(&finding["reason"]),
        ));
        for revision in export
            .finding_revisions
            .iter()
            .filter(|r| r["finding_id"] == id)
        {
            out.push_str(&format!(
                "- Step: {} — {}{}\n",
                revision["state"].as_str().unwrap_or(""),
                inline(&revision["reason"]),
                if revision["environmental_failure"] == 1
                    || revision["environmental_failure"] == true
                {
                    " (environmental failure)"
                } else {
                    ""
                }
            ));
        }
        for evidence in export.evidence.iter().filter(|e| e["finding_id"] == id) {
            out.push_str(&format!(
                "- Evidence `{}`: `{}` exit {}{}{}\n",
                evidence["id"].as_str().unwrap_or(""),
                inline(&evidence["invocation"]),
                evidence["exit_code"]
                    .as_i64()
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "none".into()),
                evidence["observed"]
                    .as_str()
                    .map(|o| format!("; observed: {}", inline(&json!(shorten(o, OBSERVED_BYTES)))))
                    .unwrap_or_default(),
                evidence["error"]
                    .as_str()
                    .map(|e| format!("; error: {}", inline(&json!(e))))
                    .unwrap_or_default(),
            ));
        }
    }
    let patches: Vec<&Value> = export
        .patches
        .iter()
        .filter(|p| matches!(p["state"].as_str(), Some("rejected" | "withdrawn")))
        .collect();
    if !patches.is_empty() {
        out.push_str("\n## Patches not taken\n\n");
        for patch in patches {
            out.push_str(&format!(
                "- `{}` {}: {} — {}\n",
                patch["id"].as_str().unwrap_or(""),
                patch["state"].as_str().unwrap_or(""),
                inline(&patch["summary"]),
                inline(&patch["reason"]),
            ));
        }
    }
    let earlier: Vec<&Value> = earlier
        .iter()
        .filter(|m| matches!(m["state"].as_str(), Some("discarded" | "inconclusive")))
        .collect();
    if !earlier.is_empty() {
        out.push_str("\n## Earlier teams on this workspace\n\n");
        for item in earlier {
            out.push_str(&format!(
                "- {} (task `{}`, snapshot `{}`): {} — {}\n",
                item["state"].as_str().unwrap_or(""),
                item["task_id"].as_str().unwrap_or(""),
                item["valid_for_snapshot"].as_str().unwrap_or("unknown"),
                inline(&item["claim"]),
                inline(&item["reason"]),
            ));
        }
    }
    out
}
