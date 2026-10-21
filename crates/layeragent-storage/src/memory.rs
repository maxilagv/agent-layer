use super::*;
use hnsw_rs::prelude::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub(crate) struct VectorIndex {
    revision: i64,
    dimensions: usize,
    index: Hnsw<'static, f32, DistCosine>,
    rows: BTreeMap<usize, Value>,
}
impl Storage {
    pub async fn put_vector(&self, id: i64, model: &str, vector: Vec<f32>) -> Result<()> {
        ensure!(
            !vector.is_empty()
                && vector.len() <= 4096
                && vector.iter().all(|v| v.is_finite())
                && vector.iter().any(|v| *v != 0.0),
            "invalid embedding"
        );
        let model = model.to_string();
        self.write(move|c|{let tx=c.transaction()?;tx.execute("INSERT OR IGNORE INTO vector_models(model,dimensions) VALUES(?1,?2)",params![model,vector.len() as i64])?;let dimensions:i64=tx.query_row("SELECT dimensions FROM vector_models WHERE model=?1",[&model],|r|r.get(0))?;ensure!(dimensions==vector.len() as i64,"embedding dimensions changed; use a new model identifier");tx.execute("INSERT INTO vectors(memory_id,model,embedding) VALUES(?1,?2,?3) ON CONFLICT(memory_id,model) DO UPDATE SET embedding=excluded.embedding",params![id,model,serde_json::to_string(&vector)?])?;tx.commit()?;Ok(())}).await
    }
    pub async fn unembedded(
        &self,
        principal: &str,
        workspace: &str,
        model: &str,
    ) -> Result<Vec<(i64, String)>> {
        let (p, w, m) = (
            principal.to_string(),
            workspace.to_string(),
            model.to_string(),
        );
        self.read(move|c|{let mut q=c.prepare("SELECT id,content FROM memories WHERE principal=?1 AND (workspace=?2 OR workspace='*') AND NOT EXISTS(SELECT 1 FROM memories replacement WHERE replacement.supersedes=memories.id) AND NOT EXISTS(SELECT 1 FROM vectors WHERE memory_id=memories.id AND model=?3) ORDER BY id DESC LIMIT 16")?;Ok(q.query_map(params![p,w,m],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await
    }
    pub async fn search_hybrid(
        &self,
        principal: &str,
        workspace: &str,
        query: &str,
        model: &str,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<Value>> {
        ensure!(
            vector.iter().all(|v| v.is_finite())
                && !vector.is_empty()
                && vector.iter().any(|v| *v != 0.0),
            "invalid query vector"
        );
        let lexical = self
            .search(principal, workspace, query, limit.max(20))
            .await?;
        let (p, w, m) = (
            principal.to_string(),
            workspace.to_string(),
            model.to_string(),
        );
        let key = format!("{p}\0{w}\0{m}");
        let revision = self
            .read(|c| {
                Ok(
                    c.query_row("SELECT (SELECT revision FROM vector_revision WHERE id=1)+(SELECT revision FROM memory_revision WHERE id=1)", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .await?;
        let rebuild = !self
            .vector_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("vector cache poisoned"))?
            .get(&key)
            .is_some_and(|i| i.revision == revision);
        if rebuild {
            let rows=self.read(move|c|{let mut q=c.prepare("SELECT memories.id,memories.content,vectors.embedding FROM memories JOIN vectors ON memories.id=vectors.memory_id WHERE principal=?1 AND (workspace=?2 OR workspace='*') AND model=?3 AND NOT EXISTS(SELECT 1 FROM memories replacement WHERE replacement.supersedes=memories.id)")?;Ok(q.query_map(params![p,w,m],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            let dimensions = vector.len();
            let index = tokio::task::spawn_blocking(move || {
                let hnsw = Hnsw::new(16, rows.len().max(1), 16, 100, DistCosine {});
                let mut data = BTreeMap::new();
                for (id, content, raw) in rows {
                    let v: Vec<f32> = serde_json::from_str(&raw)?;
                    ensure!(
                        v.len() == dimensions,
                        "stored embedding dimensions mismatch"
                    );
                    hnsw.insert((&v, id as usize));
                    data.insert(id as usize, json!({"id":id,"content":content}));
                }
                Ok::<_, anyhow::Error>(VectorIndex {
                    revision,
                    dimensions,
                    index: hnsw,
                    rows: data,
                })
            })
            .await??;
            let mut cache = self
                .vector_cache
                .lock()
                .map_err(|_| anyhow::anyhow!("vector cache poisoned"))?;
            if cache.len() >= 8 {
                cache.pop_first();
            }
            cache.insert(key.clone(), index);
        }
        let mut scores: BTreeMap<i64, (f64, Value)> = BTreeMap::new();
        for (rank, row) in lexical.into_iter().enumerate() {
            if let Some(id) = row["id"].as_i64() {
                scores.insert(id, (1.0 / (61 + rank) as f64, row));
            }
        }
        let cache = self
            .vector_cache
            .lock()
            .map_err(|_| anyhow::anyhow!("vector cache poisoned"))?;
        if let Some(index) = cache.get(&key) {
            ensure!(
                index.dimensions == vector.len(),
                "query vector dimensions mismatch"
            );
            for (rank, neighbor) in index
                .index
                .search(&vector, limit.max(20), 64)
                .iter()
                .enumerate()
            {
                if let Some(row) = index.rows.get(&neighbor.get_origin_id()) {
                    let item = scores
                        .entry(neighbor.get_origin_id() as i64)
                        .or_insert((0.0, row.clone()));
                    item.0 += 1.0 / (61 + rank) as f64;
                }
            }
        }
        let mut values = scores.into_values().collect::<Vec<_>>();
        values.sort_by(|a, b| b.0.total_cmp(&a.0));
        Ok(values
            .into_iter()
            .take(limit.min(50))
            .map(|(score, mut row)| {
                row["rrf_score"] = json!(score);
                row
            })
            .collect())
    }
    pub async fn relate(
        &self,
        principal: &str,
        workspace: &str,
        source: i64,
        relation: &str,
        target: i64,
    ) -> Result<()> {
        let (p, w, r) = (
            principal.to_string(),
            workspace.to_string(),
            relation.to_string(),
        );
        self.write(move|c|{let count:i64=c.query_row("SELECT count(*) FROM memories WHERE id IN (?1,?2) AND principal=?3 AND workspace=?4",params![source,target,p,w],|r|r.get(0))?;ensure!(count==if source==target{1}else{2},"relationship contains inaccessible facts");c.execute("INSERT INTO relations(principal,workspace,source,relation,target) VALUES(?1,?2,?3,?4,?5)",params![p,w,source,r,target])?;Ok(())}).await
    }
    pub async fn related(&self, principal: &str, workspace: &str, id: i64) -> Result<Vec<Value>> {
        let (p, w) = (principal.to_string(), workspace.to_string());
        self.read(move |db| {
            let accessible: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1 AND principal=?2 AND workspace=?3)",params![id,p,w],|r| r.get(0))?;
            ensure!(accessible, "memory inaccessible");
            let mut query = db.prepare("WITH RECURSIVE neighbors(id,depth) AS (SELECT ?1,0 UNION SELECT CASE WHEN r.source=n.id THEN r.target ELSE r.source END,n.depth+1 FROM neighbors n JOIN relations r ON (r.source=n.id OR r.target=n.id) WHERE n.depth<2 AND r.principal=?2 AND r.workspace=?3 LIMIT 256) SELECT m.id,m.content,MIN(n.depth) FROM neighbors n JOIN memories m ON m.id=n.id WHERE m.id!=?1 AND m.principal=?2 AND m.workspace=?3 AND NOT EXISTS(SELECT 1 FROM memories replacement WHERE replacement.supersedes=m.id) GROUP BY m.id ORDER BY MIN(n.depth),m.id LIMIT 50")?;
            Ok(query.query_map(params![id,p,w],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"content":r.get::<_,String>(1)?,"hops":r.get::<_,i64>(2)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }
}

struct Worker {
    child: tokio::process::Child,
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
    model: String,
    used: std::time::Instant,
}
#[derive(Clone)]
pub struct EmbeddingWorker {
    inner: Arc<tokio::sync::Mutex<Option<Worker>>>,
    cache: PathBuf,
    idle: Arc<std::sync::atomic::AtomicU64>,
}
impl EmbeddingWorker {
    pub fn new(cache: PathBuf, idle_seconds: u64) -> Self {
        let inner = Arc::new(tokio::sync::Mutex::new(None::<Worker>));
        let weak = Arc::downgrade(&inner);
        let idle = Arc::new(std::sync::atomic::AtomicU64::new(idle_seconds));
        let worker_idle = idle.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let Some(inner) = weak.upgrade() else { break };
                let mut slot = inner.lock().await;
                if slot.as_ref().is_some_and(|w| {
                    w.used.elapsed().as_secs()
                        >= worker_idle.load(std::sync::atomic::Ordering::Relaxed)
                }) && let Some(mut worker) = slot.take()
                {
                    let _ = worker.child.kill().await;
                }
            }
        });
        Self { inner, cache, idle }
    }
    pub fn set_idle_seconds(&self, idle: u64) {
        self.idle.store(idle, std::sync::atomic::Ordering::Relaxed);
    }
    pub async fn embed(&self, model: &str, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        ensure!(
            !texts.is_empty() && texts.len() <= 32 && texts.iter().all(|s| s.len() <= 16_384),
            "embedding request exceeds limits"
        );
        let mut slot = self.inner.lock().await;
        if slot.as_ref().is_none_or(|w| w.model != model) {
            if let Some(mut old) = slot.take() {
                let _ = old.child.kill().await;
            }
            std::fs::create_dir_all(&self.cache)?;
            let mut child = tokio::process::Command::new(std::env::current_exe()?)
                .args(["__embed_worker", model])
                .arg(&self.cache)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            let input = child
                .stdin
                .take()
                .context("embedding worker stdin missing")?;
            let output = BufReader::new(
                child
                    .stdout
                    .take()
                    .context("embedding worker stdout missing")?,
            );
            *slot = Some(Worker {
                child,
                input,
                output,
                model: model.into(),
                used: std::time::Instant::now(),
            });
        }
        // Owning the worker during IO makes cancellation kill the child and discard a
        // partial response, so it cannot be mistaken for another principal's reply.
        let mut worker = slot.take().context("embedding worker unavailable")?;
        worker.used = std::time::Instant::now();
        let result=tokio::time::timeout(std::time::Duration::from_secs(180),async{worker.input.write_all(format!("{}\n",serde_json::to_string(&texts)?).as_bytes()).await?;worker.input.flush().await?;let mut line=String::new();worker.output.read_line(&mut line).await?;ensure!(line.len()<=2*1024*1024,"embedding response too large");let response:Value=serde_json::from_str(&line).context("embedding worker unavailable; build with --features embeddings and install ONNX Runtime")?;if let Some(error)=response["error"].as_str(){anyhow::bail!("{error}");}Ok::<_,anyhow::Error>(serde_json::from_value::<Vec<Vec<f32>>>(response["vectors"].clone())?)}).await;
        match result {
            Ok(Ok(v)) => {
                ensure!(v.len() == texts.len(), "embedding response count mismatch");
                *slot = Some(worker);
                Ok(v)
            }
            other => {
                let _ = worker.child.kill().await;
                match other {
                    Ok(Err(e)) => Err(e),
                    Err(_) => anyhow::bail!("embedding worker timed out"),
                    _ => unreachable!(),
                }
            }
        }
    }
}
#[cfg(feature = "embeddings")]
pub fn embedding_worker_main(model: &str, cache: PathBuf) -> Result<()> {
    use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};
    use std::io::{BufRead, Write};
    let model = match model {
        "multilingual-e5-small" => EmbeddingModel::MultilingualE5Small,
        "bge-small-en-v1.5" => EmbeddingModel::BGESmallENV15,
        "all-minilm-l6-v2" => EmbeddingModel::AllMiniLML6V2,
        _ => anyhow::bail!("unsupported local embedding model"),
    };
    let mut engine = TextEmbedding::try_new(
        TextInitOptions::new(model)
            .with_cache_dir(cache)
            .with_show_download_progress(false)
            .with_intra_threads(2),
    )?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        ensure!(line.len() <= 524_288, "embedding request too large");
        let texts: Vec<String> = serde_json::from_str(&line)?;
        ensure!(texts.len() <= 32, "too many embedding inputs");
        let embeddings = engine.embed(texts, None)?;
        println!("{}", json!({"vectors":embeddings}));
        std::io::stdout().flush()?;
    }
    Ok(())
}
#[cfg(not(feature = "embeddings"))]
pub fn embedding_worker_main(_model: &str, _cache: PathBuf) -> Result<()> {
    anyhow::bail!("embedding support is optional; build layeragent with --features embeddings")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn semantic_retrieval_is_scoped_and_fused() -> Result<()> {
        let d = tempfile::tempdir()?;
        let s = Storage::open(d.path(), "t")?;
        let a = s
            .remember("alice", "repo", "network isolation", "security")
            .await?;
        let b = s
            .remember("bob", "repo", "secret knowledge", "private")
            .await?;
        s.put_vector(a, "test", vec![1., 0., 0.]).await?;
        s.put_vector(b, "test", vec![1., 0., 0.]).await?;
        let rows = s
            .search_hybrid("alice", "repo", "sandbox", "test", vec![1., 0., 0.], 5)
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], a);
        Ok(())
    }
}
