# 04 · Persistent state and memory

SQLite stores tenant-scoped sessions, messages, events, tool intent/results, queues, facts, evidence and child-agent records. Each tenant has its own database under the selected profile.

| Property | Implementation |
| --- | --- |
| Durability | WAL, synchronous FULL, transactional writes |
| Concurrency | Serialized writer and bounded reader pool |
| Migrations | Ordered `user_version` steps; pre-upgrade database snapshot |
| Lexical retrieval | FTS5/BM25, scoped queries and revision-invalidated bounded cache |
| Vector retrieval | Optional on-demand FastEmbed worker, HNSW and rank fusion |
| Corrections | Superseding fact history instead of destructive replacement |
| Relationships | Scoped graph-neighbor retrieval |
| Backups | Separate authenticated offline snapshot/restore workflow |

## Plaintext boundaries

Messages, tool-result copies, memory and textual artifacts are sanitized. Tool identity uses a digest of original arguments. Clean signed provider units remain exact; detected sensitive signed units are rejected. Binary artifacts remain byte-exact and are not OCR-redacted.

Existing historical exports and backups are not retroactively scrubbed. Review them and rotate credentials if prior exposure is suspected.

## Optional embeddings

```bash
cargo build --release --locked -p layeragent-cli --features embeddings
```

Set `memory.embeddings = true` after installing compatible ONNX Runtime and, when needed, `ORT_DYLIB_PATH`. Model artifacts may be downloaded to the private cache. Idle workers unload. Missing support emits degradation evidence and falls back to lexical retrieval. Building the feature does not prove real inference quality or speed.

## Operations

Keep the database on a local filesystem. WAL is not a distributed database or a shared-network-filesystem synchronization mechanism. Use [backup/restore](13-backup-and-recovery.md); copying an active database file is not the supported procedure.
