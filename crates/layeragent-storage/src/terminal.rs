use crate::Storage;
use anyhow::{Context, Result, ensure};
use layeragent_protocol::{Message, SessionPreferences, new_id, now_ms};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

impl Storage {
    pub async fn session_preferences(
        &self,
        id: &str,
        principal: &str,
    ) -> Result<SessionPreferences> {
        let (id, principal) = (id.to_string(), principal.to_string());
        self.read(move |db| {
            let (effort, bridge): (String, bool) = db.query_row(
                "SELECT effort,bridge FROM sessions WHERE id=?1 AND principal=?2",
                params![id, principal],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            Ok(SessionPreferences {
                effort: effort.parse()?,
                bridge,
            })
        })
        .await
        .context("session not found or inaccessible")
    }

    pub async fn set_session_preferences(
        &self,
        id: &str,
        principal: &str,
        preferences: SessionPreferences,
    ) -> Result<()> {
        let (id, principal) = (id.to_string(), principal.to_string());
        self.write(move |db| {
            let tx = db.transaction()?;
            let changed = tx.execute(
                "UPDATE sessions SET effort=?3,bridge=?4 WHERE id=?1 AND principal=?2",
                params![
                    id,
                    principal,
                    preferences.effort.as_str(),
                    preferences.bridge
                ],
            )?;
            ensure!(changed == 1, "session not found or inaccessible");
            tx.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn history_since(&self, session: &str, after: i64) -> Result<Vec<(i64, Message)>> {
        let session = session.to_string();
        self.read(move |db| {
            let mut query = db.prepare(
                "SELECT seq,body FROM messages WHERE session_id=?1 AND seq>?2 ORDER BY seq",
            )?;
            let rows = query
                .query_map(params![session, after], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(seq, body)| Ok((seq, serde_json::from_str(&body)?)))
                .collect()
        })
        .await
    }

    pub async fn latest_checkpoint(&self, session: &str) -> Result<Option<Value>> {
        let session = session.to_string();
        self.read(move |db| {
            let row = db.query_row(
                "SELECT id,session_id,through_seq,summary,kind,model,usage,history_sha256,created_at FROM context_checkpoints WHERE session_id=?1 ORDER BY through_seq DESC LIMIT 1",
                [&session],
                |row| Ok(json!({
                    "id":row.get::<_,String>(0)?,"session_id":row.get::<_,String>(1)?,
                    "through_seq":row.get::<_,i64>(2)?,"summary":row.get::<_,String>(3)?,
                    "kind":row.get::<_,String>(4)?,"model":row.get::<_,Option<String>>(5)?,
                    "usage":serde_json::from_str::<Value>(&row.get::<_,String>(6)?).unwrap_or(Value::Null),
                    "history_sha256":row.get::<_,String>(7)?,"created_at":row.get::<_,i64>(8)?
                })),
            ).optional()?;
            Ok(row)
        }).await
    }

    pub async fn save_checkpoint(
        &self,
        session: &str,
        through_seq: i64,
        summary: &str,
        kind: &str,
        model: Option<&str>,
        usage: &Value,
    ) -> Result<Value> {
        ensure!(
            through_seq > 0 && !summary.trim().is_empty() && summary.len() <= 262_144,
            "invalid context checkpoint"
        );
        ensure!(
            matches!(kind, "semantic" | "mechanical"),
            "invalid checkpoint kind"
        );
        let history = self.history_since(session, 0).await?;
        ensure!(
            history.iter().any(|(seq, _)| *seq == through_seq),
            "checkpoint sequence is outside the session"
        );
        let covered = history
            .into_iter()
            .take_while(|(seq, _)| *seq <= through_seq)
            .map(|(_, message)| message)
            .collect::<Vec<_>>();
        let history_sha256 = hex::encode(Sha256::digest(serde_json::to_vec(&covered)?));
        if let Some(existing) = self.latest_checkpoint(session).await?
            && existing["through_seq"].as_i64() == Some(through_seq)
            && existing["history_sha256"].as_str() == Some(&history_sha256)
        {
            return Ok(existing);
        }
        let checkpoint = json!({"id":new_id(),"session_id":session,"through_seq":through_seq,"summary":layeragent_protocol::redact::redact_text(summary),"kind":kind,"model":model,"usage":usage,"history_sha256":history_sha256,"created_at":now_ms()});
        let stored = checkpoint.clone();
        self.write(move |db| {
            db.execute(
                "INSERT OR IGNORE INTO context_checkpoints(id,session_id,through_seq,summary,kind,model,usage,history_sha256,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![stored["id"].as_str(),stored["session_id"].as_str(),stored["through_seq"].as_i64(),stored["summary"].as_str(),stored["kind"].as_str(),stored["model"].as_str(),stored["usage"].to_string(),stored["history_sha256"].as_str(),stored["created_at"].as_i64()],
            )?;
            Ok(())
        }).await?;
        Ok(checkpoint)
    }

    pub async fn maintenance_start(&self, session: &str, kind: &str) -> Result<Value> {
        let operation = json!({"id":new_id(),"session_id":session,"kind":kind,"state":"running","created_at":now_ms()});
        let stored = operation.clone();
        self.write(move |db| {
            let tx = db.transaction()?;
            let active: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM maintenance_operations WHERE session_id=?1 AND state='running')",
                [&stored["session_id"].as_str().unwrap_or_default()], |row| row.get(0),
            )?;
            ensure!(!active, "another maintenance operation is active");
            tx.execute("INSERT INTO maintenance_operations(id,session_id,kind,state,created_at,updated_at) VALUES(?1,?2,?3,'running',?4,?4)",params![stored["id"].as_str(),stored["session_id"].as_str(),stored["kind"].as_str(),stored["created_at"].as_i64()])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(operation)
    }

    pub async fn maintenance_finish(
        &self,
        id: &str,
        result: Option<&Value>,
        error: Option<&str>,
    ) -> Result<()> {
        let (id, result, error) = (
            id.to_string(),
            result.map(Value::to_string),
            error.map(layeragent_protocol::redact::redact_text),
        );
        self.write(move |db| {
            db.execute("UPDATE maintenance_operations SET state=?2,result=?3,error=?4,updated_at=?5 WHERE id=?1 AND state='running'",params![id,if error.is_some(){"failed"}else{"completed"},result,error,now_ms()])?;
            Ok(())
        }).await
    }

    pub async fn maintenance_status(&self, session: &str) -> Result<Vec<Value>> {
        let session = session.to_string();
        self.read(move |db| {
            let mut query = db.prepare("SELECT id,kind,state,result,error,created_at,updated_at FROM maintenance_operations WHERE session_id=?1 ORDER BY created_at DESC LIMIT 20")?;
            let rows = query.query_map([session], |row| Ok(json!({"id":row.get::<_,String>(0)?,"kind":row.get::<_,String>(1)?,"state":row.get::<_,String>(2)?,"result":row.get::<_,Option<String>>(3)?.and_then(|v|serde_json::from_str::<Value>(&v).ok()),"error":row.get::<_,Option<String>>(4)?,"created_at":row.get::<_,i64>(5)?,"updated_at":row.get::<_,i64>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await
    }

    pub async fn bridge_send(
        &self,
        root: &str,
        sender: Option<&str>,
        sender_name: &str,
        recipient: Option<&str>,
        recipient_name: &str,
        content: &str,
    ) -> Result<Value> {
        ensure!(
            !content.trim().is_empty() && content.len() <= 65_536,
            "bridge message must contain 1..65536 bytes"
        );
        let message = json!({"id":new_id(),"root_turn_id":root,"sender_agent_id":sender,"sender_name":sender_name,"recipient_agent_id":recipient,"recipient_name":recipient_name,"content":layeragent_protocol::redact::redact_text(content),"created_at":now_ms()});
        let stored = message.clone();
        self.write(move |db| {
            let tx = db.transaction()?;
            let sequence: i64 = tx.query_row("SELECT COALESCE(MAX(sequence),0)+1 FROM bridge_messages WHERE root_turn_id=?1 AND sender_agent_id IS ?2",params![stored["root_turn_id"].as_str(),stored["sender_agent_id"].as_str()],|row|row.get(0))?;
            ensure!(sequence <= 1024, "bridge sender message limit reached");
            tx.execute("INSERT INTO bridge_messages(id,root_turn_id,sender_agent_id,sender_name,recipient_agent_id,recipient_name,sequence,content,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![stored["id"].as_str(),stored["root_turn_id"].as_str(),stored["sender_agent_id"].as_str(),stored["sender_name"].as_str(),stored["recipient_agent_id"].as_str(),stored["recipient_name"].as_str(),sequence,stored["content"].as_str(),stored["created_at"].as_i64()])?;
            tx.commit()?;
            Ok(())
        }).await?;
        Ok(message)
    }

    pub async fn bridge_receive(
        &self,
        root: &str,
        recipient: Option<&str>,
    ) -> Result<Option<Value>> {
        let (root, recipient) = (root.to_string(), recipient.map(str::to_string));
        self.write(move |db| {
            let tx = db.transaction()?;
            let row = tx.query_row("SELECT id,sender_agent_id,sender_name,recipient_agent_id,recipient_name,sequence,content,created_at FROM bridge_messages WHERE root_turn_id=?1 AND state='pending' AND recipient_agent_id IS ?2 AND sender_agent_id IS NOT ?2 ORDER BY created_at,sequence LIMIT 1",params![root,recipient],|row|Ok(json!({"id":row.get::<_,String>(0)?,"sender_agent_id":row.get::<_,Option<String>>(1)?,"sender_name":row.get::<_,String>(2)?,"recipient_agent_id":row.get::<_,Option<String>>(3)?,"recipient_name":row.get::<_,String>(4)?,"sequence":row.get::<_,i64>(5)?,"content":row.get::<_,String>(6)?,"created_at":row.get::<_,i64>(7)?}))).optional()?;
            if let Some(message) = &row {
                let changed = tx.execute("UPDATE bridge_messages SET state='delivered',delivered_at=?2 WHERE id=?1 AND state='pending'",params![message["id"].as_str(),now_ms()])?;
                ensure!(changed == 1, "bridge message was claimed concurrently");
            }
            tx.commit()?;
            Ok(row)
        }).await
    }

    pub async fn bridge_pending(&self, root: &str) -> Result<usize> {
        let root = root.to_string();
        self.read(move |db| {
            Ok(db.query_row(
                "SELECT COUNT(*) FROM bridge_messages WHERE root_turn_id=?1 AND state='pending'",
                [root],
                |row| row.get::<_, i64>(0),
            )? as usize)
        })
        .await
    }

    pub async fn memories(
        &self,
        principal: &str,
        workspace: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let (principal, workspace) = (principal.to_string(), workspace.to_string());
        let limit = limit.clamp(1, 100) as i64;
        self.read(move |db| {
            let mut query = db.prepare("SELECT id,content,tags,created_at,supersedes,retired_at,source FROM memories WHERE principal=?1 AND (workspace=?2 OR workspace='*') AND id>?3 ORDER BY id LIMIT ?4")?;
            let rows = query.query_map(params![principal,workspace,after,limit],|row|Ok(json!({"id":row.get::<_,i64>(0)?,"content":row.get::<_,String>(1)?,"tags":row.get::<_,String>(2)?,"created_at":row.get::<_,i64>(3)?,"supersedes":row.get::<_,Option<i64>>(4)?,"retired_at":row.get::<_,Option<i64>>(5)?,"source":row.get::<_,String>(6)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        }).await
    }

    pub async fn retire_memory(&self, principal: &str, workspace: &str, id: i64) -> Result<bool> {
        let (principal, workspace) = (principal.to_string(), workspace.to_string());
        self.write(move |db| {
            let changed = db.execute("UPDATE memories SET retired_at=?4 WHERE id=?1 AND principal=?2 AND workspace=?3 AND retired_at IS NULL",params![id,principal,workspace,now_ms()])?;
            if changed > 0 { db.execute("DELETE FROM vectors WHERE memory_id=?1",[id])?; }
            Ok(changed > 0)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn checkpoints_preserve_history_and_preferences_are_scoped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open(dir.path(), "test")?;
        let session = storage.create_session("alice", dir.path()).await?;
        storage
            .message(
                &session.id,
                &Message::text("user", "remember this decision"),
            )
            .await?;
        storage
            .message(&session.id, &Message::text("assistant", "verified"))
            .await?;
        let rows = storage.history_since(&session.id, 0).await?;
        let checkpoint = storage
            .save_checkpoint(
                &session.id,
                rows[1].0,
                "decision",
                "mechanical",
                None,
                &json!({}),
            )
            .await?;
        let repeated = storage
            .save_checkpoint(
                &session.id,
                rows[1].0,
                "decision",
                "mechanical",
                None,
                &json!({}),
            )
            .await?;
        assert_eq!(checkpoint["id"], repeated["id"]);
        assert_eq!(storage.history(&session.id).await?.len(), 2);
        assert!(
            storage
                .history_since(&session.id, rows[1].0)
                .await?
                .is_empty()
        );
        assert!(
            storage
                .session_preferences(&session.id, "bob")
                .await
                .is_err()
        );
        let preferences = SessionPreferences {
            effort: layeragent_protocol::EffortLevel::High,
            bridge: false,
        };
        storage
            .set_session_preferences(&session.id, "alice", preferences)
            .await?;
        assert_eq!(
            storage
                .session_preferences(&session.id, "alice")
                .await?
                .effort,
            layeragent_protocol::EffortLevel::High
        );
        storage.maintenance_start(&session.id, "compact").await?;
        assert!(
            storage
                .maintenance_start(&session.id, "compact")
                .await
                .is_err()
        );
        storage.recover().await?;
        assert_eq!(
            storage.maintenance_status(&session.id).await?[0]["state"],
            "failed"
        );
        let memory = storage
            .remember("alice", "repo", "quartz setting", "manual")
            .await?;
        assert!(!storage.retire_memory("bob", "repo", memory).await?);
        assert!(storage.retire_memory("alice", "repo", memory).await?);
        assert!(
            storage
                .search("alice", "repo", "quartz", 10)
                .await?
                .is_empty()
        );
        Ok(())
    }
}
