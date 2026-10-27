use anyhow::{Context, Result, ensure};
use layeragent_protocol::{Event, Evidence, Message, Session, new_id, now_ms, validate_name};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{Semaphore, mpsc, oneshot};
pub mod backup;
mod memory;
mod tasks;
pub use memory::{EmbeddingWorker, embedding_worker_main};
pub use tasks::{TaskConflict, TaskCreate};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;
/// A delegated child agent as recorded for audit and recovery.
#[derive(Clone, Debug)]
pub struct AgentRow {
    pub id: String,
    pub root_turn_id: String,
    pub parent_turn_id: String,
    pub parent_agent_id: Option<String>,
    pub name: String,
    pub task: String,
    pub depth: usize,
    pub workspace: String,
    pub isolation: String,
}
#[derive(Clone)]
pub struct Storage {
    tx: mpsc::Sender<Job>,
    path: Arc<PathBuf>,
    artifacts: Arc<PathBuf>,
    readers: Arc<Semaphore>,
    reader_pool: Arc<std::sync::Mutex<Vec<Connection>>>,
    vector_cache: Arc<std::sync::Mutex<BTreeMap<String, memory::VectorIndex>>>,
    search_cache: Arc<std::sync::Mutex<SearchCache>>,
}
use std::collections::BTreeMap;

#[derive(Default)]
struct SearchCache {
    revision: i64,
    // Both the entry count and serialized bytes are bounded. Keys include scope.
    entries: BTreeMap<String, Vec<Value>>,
    bytes: usize,
}

/// Ordered, append-only schema steps. Entry `n` upgrades `user_version` n to n+1.
/// Steps must be idempotent enough to survive a crash before the version bump.
const MIGRATIONS: &[&str] = &[
    include_str!("schema.sql"),
    include_str!("migrations/002_agents.sql"),
    include_str!("migrations/003_tasks.sql"),
    include_str!("migrations/004_task_cancellation_trigger.sql"),
];

pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

fn migrate(connection: &mut Connection, path: &Path) -> Result<()> {
    let current: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        current <= SCHEMA_VERSION,
        "database was created by a newer LayerAgent version"
    );
    if current == SCHEMA_VERSION {
        return Ok(());
    }
    if current > 0 {
        // Snapshot before touching an existing database so a failed upgrade is recoverable.
        let name = format!("state.db.pre-v{SCHEMA_VERSION}.bak");
        let mut snapshot = Connection::open(path.with_file_name(name))?;
        rusqlite::backup::Backup::new(connection, &mut snapshot)?.run_to_completion(
            128,
            std::time::Duration::from_millis(5),
            None,
        )?;
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let step = connection.transaction()?;
        step.execute_batch(sql)?;
        step.pragma_update(None, "user_version", index as i64 + 1)?;
        step.commit()?;
    }
    let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(version == SCHEMA_VERSION, "unsupported database schema");
    Ok(())
}

impl Storage {
    pub fn open(home: &Path, tenant: &str) -> Result<Self> {
        validate_name(tenant)?;
        let dir = home.join("tenants").join(tenant);
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.join("state.db");
        let artifacts = dir.join("artifacts");
        std::fs::create_dir_all(&artifacts)?;
        let mut connection = Connection::open(&path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA cache_size=-8192;")?;
        migrate(&mut connection, &path)?;
        // An interrupted host process cannot establish whether an external action completed.
        let (tx, mut rx) = mpsc::channel::<Job>(256);
        std::thread::Builder::new()
            .name(format!("layeragent-db-{tenant}"))
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut connection);
                }
            })?;
        Ok(Self {
            tx,
            path: Arc::new(path),
            artifacts: Arc::new(artifacts),
            readers: Arc::new(Semaphore::new(2)),
            reader_pool: Default::default(),
            vector_cache: Default::default(),
            search_cache: Default::default(),
        })
    }
    pub async fn write<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .await
            .map_err(|_| anyhow::anyhow!("database writer closed"))?;
        rx.await.context("database writer lost")?
    }
    pub async fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self.readers.clone().acquire_owned().await?;
        let path = self.path.clone();
        let pool = self.reader_pool.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let prior = pool
                .lock()
                .map_err(|_| anyhow::anyhow!("reader pool poisoned"))?
                .pop();
            let c = match prior {
                Some(c) => c,
                None => {
                    let c = Connection::open_with_flags(
                        &*path,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?;
                    c.busy_timeout(std::time::Duration::from_secs(5))?;
                    c.execute_batch("PRAGMA query_only=ON; PRAGMA cache_size=-2048;")?;
                    c
                }
            };
            let result = f(&c);
            pool.lock()
                .map_err(|_| anyhow::anyhow!("reader pool poisoned"))?
                .push(c);
            result
        })
        .await?
    }
    pub async fn create_session(&self, principal: &str, workspace: &Path) -> Result<Session> {
        let s = Session {
            id: new_id(),
            principal: principal.into(),
            workspace: workspace.to_string_lossy().into(),
            created_at: now_ms(),
        };
        let row = s.clone();
        self.write(move |c| {
            c.execute(
                "INSERT INTO sessions(id,principal,workspace,created_at) VALUES(?1,?2,?3,?4)",
                params![row.id, row.principal, row.workspace, row.created_at],
            )?;
            Ok(())
        })
        .await?;
        Ok(s)
    }
    pub async fn sessions(&self, principal: &str) -> Result<Vec<Session>> {
        let p = principal.to_string();
        self.read(move|c|{ let mut q=c.prepare("SELECT id,principal,workspace,created_at FROM sessions WHERE principal=?1 ORDER BY created_at DESC")?; let rows=q.query_map([p],|r|Ok(Session{id:r.get(0)?,principal:r.get(1)?,workspace:r.get(2)?,created_at:r.get(3)?}))?; Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?) }).await
    }
    pub async fn session(&self, id: &str, principal: &str) -> Result<Session> {
        let id = id.to_string();
        let p = principal.to_string();
        self.read(move|c|Ok(c.query_row("SELECT id,principal,workspace,created_at FROM sessions WHERE id=?1 AND principal=?2",params![id,p],|r|Ok(Session{id:r.get(0)?,principal:r.get(1)?,workspace:r.get(2)?,created_at:r.get(3)?}))?)).await.context("session not found or inaccessible")
    }
    pub async fn history(&self, id: &str) -> Result<Vec<Message>> {
        let id = id.to_string();
        self.read(move |c| {
            let mut q = c.prepare("SELECT body FROM messages WHERE session_id=?1 ORDER BY seq")?;
            let raw = q
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raw.into_iter()
                .map(|s| Ok(serde_json::from_str(&s)?))
                .collect()
        })
        .await
    }
    pub async fn message(&self, id: &str, m: &Message) -> Result<()> {
        let id = id.to_string();
        let body = serde_json::to_string(&layeragent_protocol::redact::sanitize_message(m)?)?;
        self.write(move |c| {
            c.execute(
                "INSERT INTO messages(session_id,body) VALUES(?1,?2)",
                params![id, body],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn start_turn(&self, id: &str, turn: &str, revision: &str) -> Result<()> {
        let (s, t, r) = (id.to_string(), turn.to_string(), revision.to_string());
        self.write(move|c|{c.execute("INSERT INTO turns(id,session_id,state,config_revision,started_at) VALUES(?1,?2,'preflight',?3,?4)",params![t,s,r,now_ms()])?;Ok(())}).await
    }
    pub async fn turn_state(&self, id: &str, state: &str) -> Result<()> {
        let (id, state) = (id.to_string(), state.to_string());
        self.write(move |c| {
            c.execute("UPDATE turns SET state=?2 WHERE id=?1", params![id, state])?;
            Ok(())
        })
        .await
    }
    pub async fn event(
        &self,
        session: &str,
        turn: Option<&str>,
        kind: &str,
        mut data: Value,
    ) -> Result<Event> {
        layeragent_protocol::redact::redact_value(&mut data);
        let (session, turn, kind) = (
            session.to_string(),
            turn.map(str::to_string),
            kind.to_string(),
        );
        self.write(move |c| {
            let at = now_ms();
            c.execute(
                "INSERT INTO events(session_id,turn_id,at,kind,data) VALUES(?1,?2,?3,?4,?5)",
                params![session, turn, at, kind, data.to_string()],
            )?;
            let seq = c.last_insert_rowid();
            if let Some(turn) = &turn {
                c.execute(
                    "INSERT INTO task_events(task_id,at,kind,data) SELECT id,?2,?3,?4 FROM tasks WHERE turn_id=?1",
                    params![turn, at, kind, data.to_string()],
                )?;
            }
            Ok(Event {
                seq,
                at,
                session_id: session,
                turn_id: turn,
                kind,
                data,
            })
        })
        .await
    }
    pub async fn events(&self, session: &str, after: i64) -> Result<Vec<Event>> {
        let session = session.to_string();
        self.read(move|c|{let mut q=c.prepare("SELECT seq,at,turn_id,kind,data FROM events WHERE session_id=?1 AND seq>?2 ORDER BY seq LIMIT 1000")?;let rows=q.query_map(params![session,after],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;rows.into_iter().map(|(seq,at,turn_id,kind,data)|Ok(Event{seq,at,session_id:session.clone(),turn_id,kind,data:serde_json::from_str(&data)?})).collect()}).await
    }
    pub async fn tool_claim(
        &self,
        turn: &str,
        call: &str,
        name: &str,
        args: &Value,
    ) -> Result<Option<Value>> {
        let mut safe_args = args.clone();
        layeragent_protocol::redact::redact_value(&mut safe_args);
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(args)?));
        let (t, id, n, a) = (
            turn.to_string(),
            call.to_string(),
            name.to_string(),
            json!({"sha256":fingerprint,"redacted":safe_args}).to_string(),
        );
        self.write(move|c| {
            let tx=c.transaction()?;
            let prior=tx.query_row("SELECT status,result,name,arguments FROM tool_runs WHERE turn_id=?1 AND call_id=?2",params![t,id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)));
            match prior { Ok((s,result,prior_name,prior_args))=> {ensure!(prior_name==n && prior_args==a,"tool identity reused for different arguments");ensure!(s=="completed","tool outcome is {s}; explicit reconciliation required"); Ok(Some(serde_json::from_str(&result.context("missing tool result")?)?))}, Err(rusqlite::Error::QueryReturnedNoRows)=>{ tx.execute("INSERT INTO tool_runs(turn_id,call_id,name,arguments,status) VALUES(?1,?2,?3,?4,'running')",params![t,id,n,a])?;tx.commit()?;Ok(None)}, Err(e)=>Err(e.into()) }
        }).await
    }
    pub async fn tool_finish(&self, turn: &str, id: &str, result: &Value) -> Result<()> {
        let mut result = result.clone();
        layeragent_protocol::redact::redact_value(&mut result);
        let (t, id, r) = (turn.to_string(), id.to_string(), result.to_string());
        self.write(move |c| {
            c.execute(
                "UPDATE tool_runs SET status='completed',result=?3 WHERE turn_id=?1 AND call_id=?2",
                params![t, id, r],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn remember(
        &self,
        principal: &str,
        workspace: &str,
        content: &str,
        tags: &str,
    ) -> Result<i64> {
        let (p, w, s, t) = (
            principal.to_string(),
            workspace.to_string(),
            layeragent_protocol::redact::redact_text(content),
            layeragent_protocol::redact::redact_text(tags),
        );
        self.write(move|c|{c.execute("INSERT INTO memories(principal,workspace,content,tags,created_at) VALUES(?1,?2,?3,?4,?5)",params![p,w,s,t,now_ms()])?;Ok(c.last_insert_rowid())}).await
    }
    pub async fn search(
        &self,
        principal: &str,
        workspace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Value>> {
        let (p, w) = (principal.to_string(), workspace.to_string());
        let query = query
            .split_whitespace()
            .take(32)
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let limit = limit.min(50) as i64;
        if query.is_empty() {
            return Ok(vec![]);
        }
        let cache = self.search_cache.clone();
        self.read(move |c| {
            let tx = c.unchecked_transaction()?;
            let revision: i64 = tx.query_row("SELECT revision FROM memory_revision WHERE id=1", [], |r| r.get(0))?;
            let key = serde_json::to_string(&(&p, &w, &query, limit))?;
            {
                let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("search cache poisoned"))?;
                if cache.revision != revision {
                    cache.entries.clear(); cache.bytes = 0; cache.revision = revision;
                }
                if let Some(rows) = cache.entries.get(&key) { return Ok(rows.clone()); }
            }
            let mut q = tx.prepare("SELECT m.id,m.content,m.tags,bm25(memories_fts) FROM memories_fts JOIN memories m ON m.id=memories_fts.rowid WHERE memories_fts MATCH ?1 AND m.principal=?2 AND (m.workspace=?3 OR m.workspace='*') AND NOT EXISTS(SELECT 1 FROM memories replacement WHERE replacement.supersedes=m.id) ORDER BY bm25(memories_fts) LIMIT ?4")?;
            let rows = q.query_map(params![query,p,w,limit], |r| Ok(json!({"id":r.get::<_,i64>(0)?,"content":r.get::<_,String>(1)?,"tags":r.get::<_,String>(2)?,"rank":r.get::<_,f64>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let bytes = key.len() + serde_json::to_vec(&rows)?.len();
            if bytes <= 131_072 {
                let mut cache = cache.lock().map_err(|_| anyhow::anyhow!("search cache poisoned"))?;
                if cache.revision == revision {
                    if cache.entries.len() >= 32 || cache.bytes + bytes > 2_097_152 { cache.entries.clear(); cache.bytes = 0; }
                    cache.bytes += bytes;
                    cache.entries.insert(key, rows.clone());
                }
            }
            Ok(rows)
        }).await
    }
    pub async fn revise_memory(
        &self,
        principal: &str,
        workspace: &str,
        supersedes: i64,
        content: &str,
        tags: &str,
    ) -> Result<i64> {
        ensure!(
            !content.trim().is_empty() && content.len() <= 262_144,
            "invalid memory content"
        );
        let (p, w, c, t) = (
            principal.to_string(),
            workspace.to_string(),
            layeragent_protocol::redact::redact_text(content),
            layeragent_protocol::redact::redact_text(tags),
        );
        self.write(move |db| {
            let tx = db.transaction()?;
            let allowed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM memories m WHERE id=?1 AND principal=?2 AND workspace=?3 AND NOT EXISTS(SELECT 1 FROM memories replacement WHERE replacement.supersedes=m.id))", params![supersedes,p,w], |r| r.get(0))?;
            ensure!(allowed, "memory is inaccessible or already superseded");
            tx.execute("INSERT INTO memories(principal,workspace,content,tags,created_at,supersedes) VALUES(?1,?2,?3,?4,?5,?6)",params![p,w,c,t,now_ms(),supersedes])?;
            let id = tx.last_insert_rowid(); tx.commit()?; Ok(id)
        }).await
    }
    pub async fn put_artifact(&self, principal: &str, bytes: Vec<u8>) -> Result<Value> {
        // Preserve binary artifacts (for example PNGs) byte-for-byte. Text and JSON
        // artifacts share the same boundary as messages, even for direct callers.
        let bytes = match std::str::from_utf8(&bytes) {
            Ok(text) => layeragent_protocol::redact::redact_text(text).into_bytes(),
            Err(_) => bytes,
        };
        let root = self.artifacts.clone();
        let result: Value = tokio::task::spawn_blocking(move || {
            let hash = hex::encode(Sha256::digest(&bytes));
            let path = root.join(&hash);
            if !path.exists() {
                let temp = root.join(format!(".{}", new_id()));
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temp)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                std::fs::rename(temp, &path)?;
                #[cfg(unix)]
                std::fs::File::open(&*root)?.sync_all()?;
            }
            Ok::<_, anyhow::Error>(json!({"artifact":hash,"bytes":bytes.len()}))
        })
        .await??;
        let hash = result["artifact"]
            .as_str()
            .context("artifact hash missing")?
            .to_string();
        let principal = principal.to_string();
        self.write(move |db| {
            db.execute(
                "INSERT OR IGNORE INTO artifact_access(hash,principal) VALUES(?1,?2)",
                params![hash, principal],
            )?;
            Ok(())
        })
        .await?;
        Ok(result)
    }
    pub async fn artifact(
        &self,
        principal: &str,
        hash: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        ensure!(
            hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "invalid artifact hash"
        );
        let (id, user) = (hash.to_string(), principal.to_string());
        let allowed = self
            .read(move |db| {
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM artifact_access WHERE hash=?1 AND principal=?2)",
                    params![id, user],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await?;
        ensure!(allowed, "artifact not found or inaccessible");
        let path = self.artifacts.join(hash);
        let mut file = tokio::fs::File::open(path).await?;
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        file.seek(std::io::SeekFrom::Start(offset as u64)).await?;
        let mut bytes = vec![0; limit.min(65_536)];
        let n = file.read(&mut bytes).await?;
        bytes.truncate(n);
        Ok(json!({"offset":offset,"text":String::from_utf8_lossy(&bytes)}))
    }
    pub async fn evidence(
        &self,
        category: &str,
        source: &str,
        hash: &str,
        offset: Option<u64>,
        description: &str,
    ) -> Result<Evidence> {
        let (cat, s, h, d) = (
            category.to_string(),
            layeragent_protocol::redact::redact_text(source),
            hash.to_string(),
            layeragent_protocol::redact::redact_text(description),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let prev: String = tx
                .query_row(
                    "SELECT hash FROM evidence ORDER BY seq DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            let mut e = Evidence {
                id: new_id(),
                at: now_ms(),
                category: cat,
                source: s,
                source_sha256: h,
                offset,
                description: d,
                previous_hash: prev,
                hash: String::new(),
            };
            e.hash = hex::encode(Sha256::digest(serde_json::to_vec(&e)?));
            tx.execute(
                "INSERT INTO evidence(hash,body) VALUES(?1,?2)",
                params![e.hash, serde_json::to_string(&e)?],
            )?;
            tx.commit()?;
            Ok(e)
        })
        .await
    }
    pub async fn ledger(&self) -> Result<Vec<Evidence>> {
        self.read(|c| {
            let mut q = c.prepare("SELECT body FROM evidence ORDER BY seq")?;
            let raw = q
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raw.into_iter()
                .map(|s| Ok(serde_json::from_str(&s)?))
                .collect()
        })
        .await
    }
    pub async fn deduplicate(&self, platform: &str, id: &str) -> Result<bool> {
        let (p, id) = (platform.to_string(), id.to_string());
        self.write(move |c| {
            Ok(c.execute(
                "INSERT OR IGNORE INTO inbound(platform,external_id,at) VALUES(?1,?2,?3)",
                params![p, id, now_ms()],
            )? == 1)
        })
        .await
    }
    pub async fn agent_create(&self, mut a: AgentRow) -> Result<()> {
        a.name = layeragent_protocol::redact::redact_text(&a.name);
        a.task = layeragent_protocol::redact::redact_text(&a.task);
        self.write(move |c| {
            c.execute(
                "INSERT INTO agents(id,root_turn_id,parent_turn_id,parent_agent_id,name,task,depth,workspace,isolation,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?10)",
                params![a.id, a.root_turn_id, a.parent_turn_id, a.parent_agent_id, a.name, a.task, a.depth as i64, a.workspace, a.isolation, now_ms()],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn agent_update(
        &self,
        id: &str,
        status: &str,
        session_id: Option<&str>,
        result: Option<&str>,
        usage: Option<&Value>,
    ) -> Result<()> {
        let (id, status) = (id.to_string(), status.to_string());
        let session_id = session_id.map(str::to_string);
        let result = result.map(layeragent_protocol::redact::redact_text);
        let usage = usage.map(Value::to_string);
        self.write(move |c| {
            let done = ["completed", "failed", "interrupted", "cancelled"].contains(&status.as_str());
            c.execute(
                "UPDATE agents SET status=?2,session_id=COALESCE(?3,session_id),result=COALESCE(?4,result),usage=COALESCE(?5,usage),finished_at=CASE WHEN ?6 THEN ?7 ELSE finished_at END WHERE id=?1",
                params![id, status, session_id, result, usage, done, now_ms()],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn agents_of_root(&self, root_turn: &str) -> Result<Vec<Value>> {
        let root = root_turn.to_string();
        self.read(move |c| {
            let mut q = c.prepare("SELECT id,parent_turn_id,parent_agent_id,name,task,depth,session_id,workspace,isolation,status,result,usage FROM agents WHERE root_turn_id=?1 ORDER BY created_at")?;
            let rows = q.query_map([root], |r| {
                Ok(json!({"id":r.get::<_,String>(0)?,"parent_turn_id":r.get::<_,String>(1)?,"parent_agent_id":r.get::<_,Option<String>>(2)?,"name":r.get::<_,String>(3)?,"task":r.get::<_,String>(4)?,"depth":r.get::<_,i64>(5)?,"session_id":r.get::<_,Option<String>>(6)?,"workspace":r.get::<_,String>(7)?,"isolation":r.get::<_,String>(8)?,"status":r.get::<_,String>(9)?,"result":r.get::<_,Option<String>>(10)?,"usage":r.get::<_,Option<String>>(11)?}))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }
    pub async fn backup(&self, to: PathBuf) -> Result<()> {
        self.write(move |c| {
            let mut destination = Connection::open(to)?;
            let b = rusqlite::backup::Backup::new(c, &mut destination)?;
            b.run_to_completion(128, std::time::Duration::from_millis(5), None)?;
            Ok(())
        })
        .await
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub async fn recover(&self) -> Result<()> {
        self.write(|c|{c.execute("UPDATE learned_trajectories SET status='interrupted' WHERE status='pending'",[])?;c.execute("UPDATE agents SET status='interrupted',finished_at=?1 WHERE status IN ('queued','running')",[now_ms()])?;c.execute("UPDATE tool_runs SET status='unknown' WHERE status='running'",[])?;c.execute("UPDATE turns SET state='interrupted' WHERE state NOT IN ('completed','failed','interrupted')",[])?;c.execute("UPDATE inbox SET status='unknown',error='Runtime stopped during processing; reconcile the session before retrying' WHERE status='processing'",[])?;c.execute("UPDATE outbox SET status='unknown',error='Delivery may have completed before the runtime stopped' WHERE status='sending'",[])?;Ok(())}).await?;
        self.recover_tasks().await?;
        Ok(())
    }
    pub async fn enqueue_inbox(
        &self,
        channel: &str,
        id: &str,
        principal: &str,
        destination: &str,
        body: &str,
    ) -> Result<bool> {
        let (c, id, p, d, b) = (
            channel.to_string(),
            id.to_string(),
            principal.to_string(),
            destination.to_string(),
            layeragent_protocol::redact::redact_text(body),
        );
        self.write(move|db|Ok(db.execute("INSERT OR IGNORE INTO inbox(channel,external_id,principal,destination,body) VALUES(?1,?2,?3,?4,?5)",params![c,id,p,d,b])?==1)).await
    }
    pub async fn claim_inbox(&self) -> Result<Option<Value>> {
        self.write(|db|{let tx=db.transaction()?;let item=tx.query_row("SELECT seq,channel,principal,destination,body FROM inbox WHERE status='pending' ORDER BY seq LIMIT 1",[],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"channel":r.get::<_,String>(1)?,"principal":r.get::<_,String>(2)?,"destination":r.get::<_,String>(3)?,"body":r.get::<_,String>(4)?})));match item{Ok(item)=>{tx.execute("UPDATE inbox SET status='processing' WHERE seq=?1",[item["id"].as_i64()])?;tx.commit()?;Ok(Some(item))},Err(rusqlite::Error::QueryReturnedNoRows)=>Ok(None),Err(e)=>Err(e.into())}}).await
    }
    pub async fn finish_inbox(
        &self,
        id: i64,
        principal: &str,
        channel: &str,
        destination: &str,
        messages: Vec<String>,
        error: Option<String>,
    ) -> Result<()> {
        let messages = messages
            .into_iter()
            .map(|text| layeragent_protocol::redact::redact_text(&text))
            .collect::<Vec<_>>();
        let error = error.map(|text| layeragent_protocol::redact::redact_text(&text));
        let (p, c, d) = (
            principal.to_string(),
            channel.to_string(),
            destination.to_string(),
        );
        self.write(move|db|{let tx=db.transaction()?;for (n,body) in messages.into_iter().enumerate(){tx.execute("INSERT INTO outbox(id,principal,channel,destination,body) VALUES(?1,?2,?3,?4,?5)",params![format!("{id}-{n}"),p,c,d,body])?;}tx.execute("UPDATE inbox SET status=?2,error=?3 WHERE seq=?1",params![id,if error.is_some(){"failed"}else{"completed"},error])?;tx.commit()?;Ok(())}).await
    }
    pub async fn claim_outbox(&self) -> Result<Option<Value>> {
        self.write(|db|{let tx=db.transaction()?;let row=tx.query_row("SELECT id,channel,destination,body,attempts FROM outbox WHERE status='pending' AND next_at<=?1 ORDER BY rowid LIMIT 1",[now_ms()],|r|Ok(json!({"id":r.get::<_,String>(0)?,"channel":r.get::<_,String>(1)?,"destination":r.get::<_,String>(2)?,"body":r.get::<_,String>(3)?,"attempts":r.get::<_,i64>(4)?})));match row{Ok(item)=>{tx.execute("UPDATE outbox SET status='sending',attempts=attempts+1 WHERE id=?1",[item["id"].as_str()])?;tx.commit()?;Ok(Some(item))},Err(rusqlite::Error::QueryReturnedNoRows)=>Ok(None),Err(e)=>Err(e.into())}}).await
    }
    pub async fn finish_outbox(
        &self,
        id: &str,
        status: &str,
        delay_ms: i64,
        error: Option<String>,
    ) -> Result<()> {
        let (id, status) = (id.to_string(), status.to_string());
        self.write(move |db| {
            db.execute(
                "UPDATE outbox SET status=?2,next_at=?3,error=?4 WHERE id=?1",
                params![id, status, now_ms() + delay_ms, error],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn inbox_status(&self, principal: &str) -> Result<Vec<Value>> {
        let p = principal.to_string();
        self.read(move|db|{let mut q=db.prepare("SELECT seq,channel,status,error FROM inbox WHERE principal=?1 ORDER BY seq DESC LIMIT 100")?;Ok(q.query_map([p],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"channel":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"error":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await
    }
    pub async fn binding(&self, channel: &str, principal: &str) -> Result<Option<String>> {
        let (c, p) = (channel.to_string(), principal.to_string());
        self.read(move |db| {
            let mut q =
                db.prepare("SELECT session_id FROM bindings WHERE platform=?1 AND principal=?2")?;
            Ok(q.query_map(params![c, p], |r| r.get::<_, String>(0))?
                .next()
                .transpose()?)
        })
        .await
    }
    pub async fn bind(&self, channel: &str, principal: &str, session: &str) -> Result<()> {
        let (c, p, s) = (
            channel.to_string(),
            principal.to_string(),
            session.to_string(),
        );
        self.write(move|db|{db.execute("INSERT OR REPLACE INTO bindings(platform,channel,principal,session_id) VALUES(?1,?1,?2,?3)",params![c,p,s])?;Ok(())}).await
    }
    pub async fn reconcile_history(&self, session: &str) -> Result<()> {
        let history = self.history(session).await?;
        let mut pending = Vec::new();
        for message in &history {
            pending.extend(message.tool_calls.iter().cloned());
            if let Some(id) = &message.tool_call_id
                && let Some(index) = pending.iter().rposition(|call| &call.id == id)
            {
                pending.remove(index);
            }
        }
        for call in &pending {
            let (id, s) = (call.id.clone(), session.to_string());
            let result=self.read(move|c|{let v=c.query_row("SELECT r.status,r.result FROM tool_runs r JOIN turns t ON r.turn_id=t.id WHERE r.call_id=?1 AND t.session_id=?2 ORDER BY t.started_at DESC LIMIT 1",params![id,s],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?)));Ok(match v{Ok((status,Some(body))) if status=="completed"=>serde_json::from_str(&body)?,_=>json!({"error":"Previous operation was interrupted; its outcome is unknown. Reconcile with the operator before repeating any side effect."})})}).await?;
            self.message(session, &Message::tool(call, &result)).await?;
        }
        Ok(())
    }
}

pub fn verify_ledger(records: &[Evidence]) -> Result<String> {
    let mut previous = String::new();
    for record in records {
        ensure!(record.previous_hash == previous, "broken evidence chain");
        let mut input = record.clone();
        input.hash.clear();
        let hash = hex::encode(Sha256::digest(serde_json::to_vec(&input)?));
        ensure!(hash == record.hash, "evidence content changed");
        previous = hash;
    }
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrations_are_versioned_and_reject_newer_databases() -> Result<()> {
        let dir = tempfile::tempdir()?;
        drop(Storage::open(dir.path(), "m")?);
        let db = dir.path().join("tenants/m/state.db");
        let c = Connection::open(&db)?;
        let v: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
        assert_eq!(v, SCHEMA_VERSION);
        c.pragma_update(None, "user_version", SCHEMA_VERSION + 1)?;
        drop(c);
        assert!(Storage::open(dir.path(), "m").is_err());
        Ok(())
    }
    #[tokio::test]
    async fn cached_recall_invalidates_and_corrections_keep_history_scoped() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let s = Storage::open(dir.path(), "test")?;
        let old = s
            .remember("alice", "repo", "port 3000", "configuration")
            .await?;
        assert_eq!(s.search("alice", "repo", "port", 10).await?[0]["id"], old);
        assert!(s.search("bob", "repo", "port", 10).await?.is_empty());
        assert!(
            s.revise_memory("bob", "repo", old, "port 5000", "")
                .await
                .is_err()
        );
        let new = s
            .revise_memory("alice", "repo", old, "port 8080", "configuration")
            .await?;
        let recall = s.search("alice", "repo", "port", 10).await?;
        assert_eq!(recall.len(), 1);
        assert_eq!(recall[0]["id"], new);
        assert!(
            s.revise_memory("alice", "repo", old, "stale replacement", "")
                .await
                .is_err()
        );
        let other = s.remember("alice", "repo", "HTTP server", "").await?;
        s.relate("alice", "repo", new, "configures", other).await?;
        assert_eq!(s.related("alice", "repo", new).await?[0]["id"], other);
        assert!(s.related("bob", "repo", new).await.is_err());
        let history: i64 = s
            .read(|db| Ok(db.query_row("SELECT count(*) FROM memories", [], |r| r.get(0))?))
            .await?;
        assert_eq!(history, 3);
        Ok(())
    }
    #[tokio::test]
    async fn durable_scoped_memory_and_idempotency() -> Result<()> {
        let d = tempfile::tempdir()?;
        let s = Storage::open(d.path(), "a")?;
        s.remember("alice", "repo", "Decidimos usar Rust y SQLite", "decision")
            .await?;
        assert_eq!(s.search("alice", "repo", "SQLite", 5).await?.len(), 1);
        assert!(s.search("bob", "repo", "SQLite", 5).await?.is_empty());
        assert!(s.tool_claim("t", "c", "write", &json!({})).await?.is_none());
        assert!(s.tool_claim("t", "c", "write", &json!({})).await.is_err());
        s.tool_finish("t", "c", &json!({"ok":true})).await?;
        assert!(s.tool_claim("t", "c", "write", &json!({})).await?.is_some());
        Ok(())
    }
    #[tokio::test]
    async fn ledger_detects_tampering() -> Result<()> {
        let d = tempfile::tempdir()?;
        let s = Storage::open(d.path(), "a")?;
        s.evidence("observed", "a.bin", "abc", Some(2), "header")
            .await?;
        s.evidence("inference", "a.bin", "abc", None, "guess")
            .await?;
        let mut records = s.ledger().await?;
        verify_ledger(&records)?;
        records[0].description = "changed".into();
        assert!(verify_ledger(&records).is_err());
        Ok(())
    }
    #[tokio::test]
    async fn artifact_hash_does_not_grant_another_principal_access() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Storage::open(dir.path(), "shared")?;
        let artifact = store
            .put_artifact("alice", b"private result".to_vec())
            .await?;
        let hash = artifact["artifact"].as_str().context("hash")?;
        assert!(store.artifact("bob", hash, 0, 100).await.is_err());
        assert_eq!(
            store.artifact("alice", hash, 0, 100).await?["text"],
            "private result"
        );
        Ok(())
    }
    #[tokio::test]
    async fn durable_queues_keep_uncertain_deliveries_without_repeating() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Storage::open(dir.path(), "a")?;
        assert!(
            store
                .enqueue_inbox("telegram", "event-1", "alice", "room", "do work")
                .await?
        );
        assert!(
            !store
                .enqueue_inbox("telegram", "event-1", "alice", "room", "do work")
                .await?
        );
        let input = store.claim_inbox().await?.context("missing queued input")?;
        assert!(store.claim_inbox().await?.is_none());
        store
            .finish_inbox(
                input["id"].as_i64().context("id")?,
                "alice",
                "telegram",
                "room",
                vec!["done".into()],
                None,
            )
            .await?;
        assert!(store.claim_outbox().await?.is_some());
        store.recover().await?;
        assert!(store.claim_outbox().await?.is_none());
        assert!(store.inbox_status("bob").await?.is_empty());
        Ok(())
    }
}
