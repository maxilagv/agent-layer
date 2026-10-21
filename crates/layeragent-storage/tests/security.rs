use anyhow::Result;
use layeragent_protocol::{Message, new_id};
use layeragent_storage::Storage;
use serde_json::json;

#[tokio::test]
async fn direct_persistence_redacts_plaintext_and_preserves_call_identity() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let store = Storage::open(home.path(), "default")?;
    let session = store.create_session("owner", workspace.path()).await?;
    let turn = new_id();
    store.start_turn(&session.id, &turn, "test").await?;
    store
        .message(
            &session.id,
            &Message::text("user", "token=abcdefgh12345678"),
        )
        .await?;
    assert!(
        !serde_json::to_string(&store.history(&session.id).await?)?.contains("abcdefgh12345678")
    );
    let call = new_id();
    let original = json!({"token":"abcdefgh12345678"});
    assert!(
        store
            .tool_claim(&turn, &call, "write_file", &original)
            .await?
            .is_none()
    );
    store.tool_finish(&turn, &call, &original).await?;
    assert!(
        store
            .tool_claim(&turn, &call, "write_file", &original)
            .await?
            .is_some()
    );
    assert!(
        store
            .tool_claim(
                &turn,
                &call,
                "write_file",
                &json!({"token":"different-12345678"})
            )
            .await
            .is_err()
    );
    let rows = store
        .read(|db| {
            Ok(db.query_row(
                "SELECT arguments || result FROM tool_runs LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await?;
    assert!(!rows.contains("abcdefgh12345678"));
    let artifact = store
        .put_artifact("owner", serde_json::to_vec(&original)?)
        .await?;
    let read = store
        .artifact(
            "owner",
            artifact["artifact"].as_str().unwrap_or(""),
            0,
            1024,
        )
        .await?;
    assert!(!read.to_string().contains("abcdefgh12345678"));
    let bytes = vec![0xff, 0x00, 0x80, 0x01];
    let binary = store.put_artifact("owner", bytes.clone()).await?;
    assert_eq!(binary["bytes"], bytes.len());
    layeragent_protocol::redact::register_secret("opaque-memory-secret-4429");
    let encoded = r#"{"value":"\u006fpaque-memory-secret-4429"}"#;
    let fact = store.remember("owner", "*", encoded, "test").await?;
    store
        .revise_memory("owner", "*", fact, encoded, "test")
        .await?;
    let memories = store
        .read(|db| {
            let mut query = db.prepare("SELECT content FROM memories")?;
            Ok(query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await?;
    assert!(
        memories
            .iter()
            .all(|text| !text.contains("opaque-memory-secret-4429") && !text.contains("u006f"))
    );
    Ok(())
}
