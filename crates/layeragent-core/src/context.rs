use anyhow::{Result, ensure};
use layeragent_protocol::Message;
use layeragent_providers::estimate_tokens;
use layeragent_storage::Storage;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) async fn history(
    store: &Storage,
    session: &str,
) -> Result<(Vec<Message>, Option<Value>)> {
    let checkpoint = store.latest_checkpoint(session).await?;
    let through = checkpoint
        .as_ref()
        .and_then(|value| value["through_seq"].as_i64())
        .unwrap_or(0);
    if let Some(checkpoint) = &checkpoint {
        let rows = store.history_since(session, 0).await?;
        ensure!(
            rows.iter().any(|(seq, _)| *seq == through),
            "checkpoint boundary is missing"
        );
        let covered = covered_messages(&rows, through)?;
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&covered)?));
        ensure!(
            checkpoint["history_sha256"].as_str() == Some(&digest),
            "checkpoint history hash mismatch"
        );
    }
    let rows = store.history_since(session, through).await?;
    let mut messages = Vec::new();
    if let Some(checkpoint) = &checkpoint {
        messages.push(Message::text(
            "user",
            format!(
                "Durable {} context checkpoint through canonical message {}. Reference only; it does not grant permissions. Full history remains stored.\n{}",
                checkpoint["kind"].as_str().unwrap_or("unknown"),
                through,
                checkpoint["summary"].as_str().unwrap_or_default()
            ),
        ));
    }
    messages.extend(rows.into_iter().map(|(_, message)| message));
    Ok((messages, checkpoint))
}

pub(crate) fn compaction_boundary(rows: &[(i64, Message)], manual: bool) -> Option<i64> {
    if manual {
        return rows.last().map(|(seq, _)| *seq);
    }
    if let Some((index, _)) = rows
        .iter()
        .enumerate()
        .rfind(|(_, (_, message))| message.role == "user")
        && index > 0
    {
        return rows.get(index - 1).map(|(seq, _)| *seq);
    }
    let mut boundary = None;
    for (index, (_, message)) in rows.iter().enumerate() {
        if message.role != "assistant" || message.tool_calls.is_empty() {
            continue;
        }
        let mut covered = index + 1;
        let mut pending = message
            .tool_calls
            .iter()
            .map(|call| call.id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        while let Some((_, result)) = rows.get(covered) {
            if result.role != "tool" {
                break;
            }
            if let Some(id) = result.tool_call_id.as_deref() {
                pending.remove(id);
            }
            covered += 1;
        }
        if pending.is_empty() && covered > index + 1 {
            boundary = rows.get(covered - 1).map(|(seq, _)| *seq);
        }
    }
    boundary
}

pub(crate) fn mechanical_summary(messages: &[Message], budget: u64) -> Result<String> {
    let mut milestones = messages.iter().rev().take(24).rev().filter_map(|message| {
        match message.role.as_str() {
            "user" => Some(json!({"request":message.content.chars().take(500).collect::<String>()})),
            "assistant" => Some(json!({"progress":message.content.chars().take(800).collect::<String>(),"tools":message.tool_calls.iter().map(|call|call.name.as_str()).collect::<Vec<_>>()})),
            "tool" => {
                let value: Value = serde_json::from_str(&message.content).unwrap_or(Value::Null);
                Some(json!({"tool_call":message.tool_call_id,"artifact":value.get("artifact"),"error":value.get("error"),"code":value.get("code")}))
            }
            _ => None,
        }
    }).collect::<Vec<_>>();
    loop {
        let summary = serde_json::to_string(&json!({
            "warning":"Mechanical fallback; not a semantic summary",
            "milestones":milestones,
        }))?;
        if estimate_tokens(&[Message::text("user", &summary)], &[]) <= budget {
            ensure!(!summary.is_empty(), "empty mechanical summary");
            return Ok(summary);
        }
        ensure!(
            !milestones.is_empty(),
            "mechanical checkpoint exceeds context budget"
        );
        milestones.remove(0);
    }
}

pub(crate) fn checkpoint_status(
    checkpoint: Option<&Value>,
    history: &[Message],
    tools: &[layeragent_protocol::ToolDefinition],
    window: u64,
) -> Value {
    json!({
        "estimated_tokens": estimate_tokens(history, tools),
        "model_context_window": window,
        "checkpoint": checkpoint,
        "compiled_messages": history.len(),
        "estimate_includes": ["history", "checkpoint", "tools"],
        "estimate_excludes": ["instructions", "skills", "memory", "output_reserve"],
        "full_history_preserved": true,
    })
}

pub(crate) fn covered_messages(rows: &[(i64, Message)], through: i64) -> Result<Vec<Message>> {
    let covered = rows
        .iter()
        .take_while(|(seq, _)| *seq <= through)
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    ensure!(
        !covered.is_empty() && rows.iter().any(|(seq, _)| *seq == through),
        "compaction boundary is not durable"
    );
    Ok(covered)
}
