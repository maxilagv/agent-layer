use anyhow::Result;
use layeragent_storage::Storage;
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<()> {
    let records = std::env::args()
        .nth(1)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100_000);
    let dir = tempfile::tempdir()?;
    let store = Storage::open(dir.path(), "benchmark")?;
    let start = Instant::now();
    store.write(move|db|{let tx=db.transaction()?;{let mut insert=tx.prepare("INSERT INTO memories(principal,workspace,content,tags,created_at) VALUES('owner','repo',?1,'fixture',0)")?;for i in 0..records{insert.execute([format!("Decision {i}: component{} uses SQLite WAL and durable memory artifact{i}",i%100)])?;}}tx.commit()?;Ok(())}).await?;
    let seed_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut measurements = vec![];
    for query in ["artifact42", "component17", "SQLite"] {
        let first = Instant::now();
        store.search("owner", "repo", query, 10).await?;
        let uncached_ms = first.elapsed().as_secs_f64() * 1000.;
        for _ in 0..5 {
            store.search("owner", "repo", query, 10).await?;
        }
        let mut values = vec![];
        for _ in 0..100 {
            let start = Instant::now();
            let _ = store.search("owner", "repo", query, 10).await?;
            values.push(start.elapsed().as_secs_f64() * 1000.);
        }
        values.sort_by(f64::total_cmp);
        measurements
            .push(serde_json::json!({"query":query,"uncached_ms":uncached_ms,"cached_p50_ms":values[49],"cached_p95_ms":values[94]}));
    }
    println!(
        "{}",
        serde_json::json!({"records":records,"seed_ms":seed_ms,"queries":measurements,"profile":"use --release for optimized results"})
    );
    Ok(())
}
