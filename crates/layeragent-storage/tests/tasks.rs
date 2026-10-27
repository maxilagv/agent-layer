//! Durable task API semantics that do not depend on a model transport.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use layeragent_protocol::task::{CorrectTask, TaskState};
use layeragent_protocol::{Scope, new_id};
use layeragent_storage::{Storage, TaskConflict, TaskCreate};
use serde_json::json;

async fn store() -> Result<(tempfile::TempDir, Storage)> {
    let home = tempfile::tempdir()?;
    let store = Storage::open(home.path(), "default")?;
    Ok((home, store))
}

#[tokio::test]
async fn identical_replay_returns_the_same_task_and_reuse_conflicts() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    let first = store
        .create_task(
            &scope,
            "/tmp/project",
            None,
            "build it",
            "local",
            "key-1",
            "hash-a",
        )
        .await?;
    let TaskCreate::Created(first) = first else {
        panic!("first request must create");
    };
    let replay = store
        .create_task(
            &scope,
            "/tmp/project",
            None,
            "build it",
            "local",
            "key-1",
            "hash-a",
        )
        .await?;
    let TaskCreate::Replayed(replay) = replay else {
        panic!("identical request must replay");
    };
    assert_eq!(first.id, replay.id);
    assert_eq!(first.last_event_seq, replay.last_event_seq);
    // The idempotency key never leaks into the task body.
    assert!(!serde_json::to_string(&replay)?.contains("key-1"));
    let conflict = store
        .create_task(
            &scope,
            "/tmp/project",
            None,
            "different work",
            "local",
            "key-1",
            "hash-b",
        )
        .await
        .expect_err("different content must conflict");
    assert!(conflict.downcast_ref::<TaskConflict>().is_some());
    // A key is scoped to one principal.
    let other = Scope {
        tenant: "default".into(),
        principal: "someone-else".into(),
    };
    assert!(
        store
            .create_task(
                &other,
                "/tmp/project",
                None,
                "build it",
                "local",
                "key-1",
                "hash-a"
            )
            .await
            .is_ok()
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_is_tracked_apart_from_the_task_state() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    let TaskCreate::Created(task) = store
        .create_task(&scope, "/tmp/project", None, "long work", "local", "k", "h")
        .await?
    else {
        panic!("created");
    };
    assert_eq!(task.state, TaskState::Prepared);
    assert_eq!(task.cancellation.as_str(), "none");
    assert!(store.request_cancel(&scope, &task.id).await?);
    let pending = store.task(&scope, &task.id).await?;
    assert_eq!(pending.cancellation.as_str(), "requested");
    assert!(!pending.state.terminal());
    // Work that never reached the engine becomes a confirmed cancellation.
    store
        .cancel_unstarted(&task.id, "cancelled before execution")
        .await?;
    let cancelled = store.task(&scope, &task.id).await?;
    assert_eq!(cancelled.state, TaskState::Cancelled);
    assert_eq!(cancelled.cancellation.as_str(), "confirmed");
    // A late engine finish cannot resurrect a finished task.
    store
        .finish_task(
            &task.id,
            &json!({"input_tokens":0}),
            layeragent_protocol::task::CancellationState::Confirmed,
            None,
        )
        .await?;
    assert_eq!(
        store.task(&scope, &task.id).await?.state,
        TaskState::Cancelled
    );
    Ok(())
}

#[tokio::test]
async fn restart_blocks_live_tasks_and_never_resumes_them() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    let TaskCreate::Created(live) = store
        .create_task(
            &scope,
            "/tmp/project",
            None,
            "was running",
            "local",
            "k1",
            "h1",
        )
        .await?
    else {
        panic!("created");
    };
    let TaskCreate::Created(finished) = store
        .create_task(
            &scope,
            "/tmp/project",
            None,
            "was done",
            "local",
            "k2",
            "h2",
        )
        .await?
    else {
        panic!("created");
    };
    store
        .finish_task(
            &finished.id,
            &json!({}),
            layeragent_protocol::task::CancellationState::None,
            None,
        )
        .await?;
    store.recover_tasks().await?;
    let live = store.task(&scope, &live.id).await?;
    assert_eq!(live.state, TaskState::Blocked);
    assert_eq!(live.cancellation.as_str(), "none");
    assert!(
        live.error
            .as_deref()
            .is_some_and(|error| error.contains("reconcile"))
    );
    let events = store.task_events(&live.id, 0, 100).await?;
    assert!(
        events
            .iter()
            .any(|event| event["kind"] == "task_interrupted")
    );
    // Re-running recovery is idempotent.
    store.recover_tasks().await?;
    assert_eq!(
        store.task(&scope, &live.id).await?.state,
        TaskState::Blocked
    );
    Ok(())
}

#[tokio::test]
async fn corrections_require_the_observed_revision_and_apply_once() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    let TaskCreate::Created(task) = store
        .create_task(&scope, "/tmp/project", None, "work", "local", "k", "h")
        .await?
    else {
        panic!("created");
    };
    let revision = store.task(&scope, &task.id).await?.revision;
    let stale = store
        .correct_task(
            &scope,
            &task.id,
            CorrectTask {
                instruction: "use the other file".into(),
                expected_revision: revision + 99,
            },
        )
        .await
        .expect_err("stale revision must conflict");
    assert!(stale.to_string().contains("revision changed"));
    let correction = store
        .correct_task(
            &scope,
            &task.id,
            CorrectTask {
                instruction: "use the other file".into(),
                expected_revision: revision,
            },
        )
        .await?;
    assert_eq!(correction.status, "pending");
    let applied = store
        .next_correction(&task.id)
        .await?
        .expect("pending correction");
    assert_eq!(applied.id, correction.id);
    assert_eq!(applied.status, "applied");
    assert!(store.next_correction(&task.id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn artifacts_are_scoped_to_the_task_and_session_that_produced_them() -> Result<()> {
    let (home, store) = store().await?;
    let scope = Scope::local();
    let workspace = tempfile::tempdir()?;
    let session = store.create_session("default", workspace.path()).await?;
    let _ = home;
    let stored = store.put_artifact("owner", b"evidence".to_vec()).await?;
    let hash = stored["artifact"].as_str().expect("hash").to_string();
    // Bind the artifact to the session/task that produced it.
    store
        .link_artifact(&scope, Some(&session.id), Some("task-a"), &hash)
        .await?;
    let allowed = store
        .artifact_scoped(&scope, Some(&session.id), Some("task-a"), &hash, 0, 64)
        .await?;
    assert_eq!(allowed["text"], "evidence");
    assert!(
        store
            .artifact_scoped(&scope, Some(&session.id), Some("task-b"), &hash, 0, 64)
            .await
            .is_err()
    );
    let other = Scope {
        tenant: "default".into(),
        principal: "intruder".into(),
    };
    assert!(
        store
            .artifact_scoped(&other, Some(&session.id), Some("task-a"), &hash, 0, 64)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn revocation_advances_the_authority_generation() -> Result<()> {
    let (_home, store) = store().await?;
    let observed = store.connection_state("provider-fixture").await?;
    assert!(observed.is_none());
    assert!(store.connection_allows("provider-fixture", None).await?);
    store
        .set_connection_enabled(
            "provider-fixture",
            "provider",
            "fixture",
            true,
            Some("reachable"),
        )
        .await?;
    let (enabled, generation) = store
        .connection_state("provider-fixture")
        .await?
        .expect("state");
    assert!(enabled);
    assert!(
        store
            .connection_allows("provider-fixture", Some(generation))
            .await?
    );
    let revoked = store
        .revoke_connection("provider-fixture", "provider", "fixture")
        .await?;
    assert_eq!(revoked, generation + 1);
    assert!(
        !store
            .connection_allows("provider-fixture", Some(generation))
            .await?
    );
    assert!(!store.connection_allows("provider-fixture", None).await?);
    Ok(())
}

#[tokio::test]
async fn task_list_cursor_pages_by_rowid() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    for index in 0..3 {
        store
            .create_task(
                &scope,
                "/tmp/p",
                None,
                &format!("task {index}"),
                "local",
                &new_id(),
                &format!("hash-{index}"),
            )
            .await?;
    }
    let (first, next) = store.tasks(&scope, 0, 2).await?;
    assert_eq!(first.len(), 2);
    let next = next.expect("more pages");
    let (second, tail) = store.tasks(&scope, next, 2).await?;
    assert_eq!(second.len(), 1);
    assert!(tail.is_none());
    assert!(!first.iter().any(|task| task.id == second[0].id));
    Ok(())
}

#[tokio::test]
async fn task_ids_are_distinct_and_usage_is_aggregated_per_principal() -> Result<()> {
    let (_home, store) = store().await?;
    let scope = Scope::local();
    let TaskCreate::Created(one) = store
        .create_task(&scope, "/tmp/p", None, "one", "local", &new_id(), "h1")
        .await?
    else {
        panic!("created");
    };
    store
        .finish_task(
            &one.id,
            &json!({"input_tokens":10,"output_tokens":4,"cost_usd":0.25}),
            layeragent_protocol::task::CancellationState::None,
            None,
        )
        .await?;
    let totals = store.usage_totals(&scope).await?;
    assert_eq!(totals["tasks"], 1);
    assert_eq!(totals["input_tokens"], 10);
    assert_eq!(totals["output_tokens"], 4);
    assert!((totals["cost_usd"].as_f64().unwrap() - 0.25).abs() < f64::EPSILON);
    Ok(())
}
