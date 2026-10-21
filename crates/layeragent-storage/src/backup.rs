//! Offline, authenticated whole-home snapshots. Archives contain only the documented
//! state allowlist; runtime sockets, environment files, keyrings and caches are omitted.
//! Callers must retain `runtime_lease` for the lifetime of every runtime using a home.
use age::{secrecy::ExposeSecret, x25519};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MAGIC: &[u8] = b"LAYERAGENT-BACKUP-1\n";
const MAX_MANIFEST: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub created_ms: i64,
    pub schema_version: i64,
    pub files: Vec<Entry>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "symlink directory refused"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}
fn reject_symlink_ancestors(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        if let Ok(meta) = fs::symlink_metadata(ancestor) {
            ensure!(!meta.file_type().is_symlink(), "symlink ancestor refused");
        }
    }
    Ok(())
}
/// Shared for runtimes, exclusive for a snapshot. Locking is advisory; unrelated
/// programs must not mutate this private home while it is being snapshotted.
fn home_lease(home: &Path, exclusive: bool) -> Result<File> {
    reject_symlink_ancestors(home)?;
    private_dir(home)?;
    let path = home.join(".backup.lock");
    ensure!(!path.is_symlink(), "symlink lock refused");
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if exclusive {
        file.try_lock_exclusive()
            .context("stop all LayerAgent runtimes before backup")?;
    } else {
        FileExt::try_lock_shared(&file).context("home is being backed up")?;
    }
    Ok(file)
}
pub fn runtime_lease(home: &Path) -> Result<File> {
    home_lease(home, false)
}

/// Generate a native age identity without printing its secret. Keep it outside
/// the runtime home and store a separate offline copy; lost keys cannot be recovered.
pub fn keygen(path: &Path) -> Result<String> {
    reject_symlink_ancestors(path)?;
    let identity = x25519::Identity::generate();
    let mut output = private_file(path)?;
    writeln!(output, "{}", identity.to_string().expose_secret())?;
    output.sync_all()?;
    Ok(identity.to_public().to_string())
}
fn identity(path: &Path) -> Result<x25519::Identity> {
    reject_symlink_ancestors(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(meta.is_file() && meta.len() < 4096, "invalid identity file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure!(
            meta.permissions().mode() & 0o077 == 0,
            "identity permissions must be private (chmod 600)"
        );
    }
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid age identity"))
}
fn parts(path: &str) -> Result<Vec<&str>> {
    ensure!(
        !path.is_empty() && !path.contains('\\') && !path.contains(':') && !path.contains('\0'),
        "invalid backup path"
    );
    ensure!(
        Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "unsafe backup path"
    );
    let p = path.split('/').collect::<Vec<_>>();
    ensure!(
        p.iter().all(|p| !p.is_empty() && *p != "." && *p != ".."),
        "invalid backup path"
    );
    for component in &p {
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        ensure!(
            !component.ends_with(['.', ' '])
                && !component.bytes().any(|b| b < 32 || b == 127)
                && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && matches!(stem.as_bytes()[3], b'1'..=b'9')),
            "nonportable backup path"
        );
    }
    Ok(p)
}
fn allowed(path: &str) -> Result<bool> {
    let p = parts(path)?;
    if p.iter().any(|name| {
        *name == ".env"
            || name.starts_with(".env.")
            || *name == "service.env"
            || *name == "credentials.json"
            || *name == "secrets.json"
            || name.ends_with(".pem")
            || name.ends_with(".key")
    }) {
        return Ok(false);
    }
    Ok(match p.as_slice() {
        ["config.toml"] | ["SOUL.md"] => true,
        ["profiles", name] => name.ends_with(".toml"),
        ["skills" | "skill-versions", _, ..] => true,
        ["data", _, "tenants", _, "state.db"] => true,
        ["data", _, "tenants", _, "artifacts", _, ..] => true,
        ["data", _, "tenants", _, "principals", _, "SOUL.md"] => true,
        [
            "data",
            _,
            "tenants",
            _,
            "principals",
            _,
            "skills" | "skill-versions",
            _,
            ..,
        ] => true,
        _ => false,
    })
}
fn database_path(path: &str) -> Result<bool> {
    Ok(matches!(
        parts(path)?.as_slice(),
        ["data", _, "tenants", _, "state.db"]
    ))
}
fn collect(home: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for child in fs::read_dir(dir)? {
        let child = child?;
        let path = child.path();
        let rel = path
            .strip_prefix(home)?
            .to_str()
            .context("non-UTF8 backup path")?
            .replace('\\', "/");
        // Do not descend into excluded top-level material (including credentials).
        let top = rel.split('/').next().unwrap_or_default();
        if !matches!(
            top,
            "config.toml" | "SOUL.md" | "profiles" | "skills" | "skill-versions" | "data"
        ) {
            continue;
        }
        let meta = fs::symlink_metadata(&path)?;
        ensure!(
            !meta.file_type().is_symlink(),
            "backup refuses symlinks in included trees"
        );
        if meta.is_dir() {
            collect(home, &path, files)?;
        } else if allowed(&rel)? {
            ensure!(meta.is_file(), "nonregular file refused");
            ensure!(files.len() < MAX_FILES, "backup file count limit exceeded");
            files.push(path);
        }
    }
    Ok(())
}
fn hash(reader: &mut dyn Read, writer: &mut dyn Write, length: u64) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut left = length;
    let mut buffer = [0; 64 * 1024];
    while left > 0 {
        let n = reader.read(&mut buffer[..left.min(65536) as usize])?;
        ensure!(n > 0, "truncated backup payload");
        writer.write_all(&buffer[..n])?;
        hasher.update(&buffer[..n]);
        left -= n as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}
/// Create a new encrypted archive; never overwrite an existing archive. Output
/// must be outside the source home. Retention is a separate explicit operation.
pub fn create(home: &Path, output: &Path, recipient: &str) -> Result<Manifest> {
    reject_symlink_ancestors(home)?;
    let home = home.canonicalize()?;
    reject_symlink_ancestors(output)?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure!(
        !parent.canonicalize()?.starts_with(&home),
        "backup output must be outside source home"
    );
    ensure!(!output.exists(), "backup output already exists");
    let _lease = home_lease(&home, true)?;
    // Also detect older daemons that predate the home lease.
    let mut legacy = Vec::new();
    let runtime = home.join("runtime");
    if runtime.is_dir() {
        ensure!(!runtime.is_symlink(), "symlink runtime refused");
        for entry in fs::read_dir(runtime)? {
            let path = entry?.path();
            if path.extension().is_some_and(|s| s == "lock") {
                ensure!(!path.is_symlink(), "symlink runtime lock refused");
                let file = OpenOptions::new().read(true).write(true).open(path)?;
                file.try_lock_exclusive()
                    .context("stop the daemon before backup")?;
                legacy.push(file);
            }
        }
    }
    let public: x25519::Recipient = recipient
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid age recipient"))?;
    let stage = tempfile::tempdir_in(&home)?;
    let mut sources = Vec::new();
    collect(&home, &home, &mut sources)?;
    sources.sort();
    let mut files = Vec::new();
    let mut total = 0_u64;
    for source in sources {
        let relative = source.strip_prefix(&home)?;
        let dest = stage.path().join(relative);
        private_dir(dest.parent().context("snapshot parent missing")?)?;
        if database_path(
            relative
                .to_str()
                .context("invalid database path")?
                .replace('\\', "/")
                .as_str(),
        )? {
            let db = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let mut snapshot = Connection::open(&dest)?;
            rusqlite::backup::Backup::new(&db, &mut snapshot)?.run_to_completion(
                128,
                std::time::Duration::from_millis(5),
                None,
            )?;
            let check: String = snapshot.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            ensure!(check == "ok", "SQLite snapshot integrity check failed");
            drop(snapshot);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
            }
        } else {
            let mut input = File::open(&source)?;
            let mut out = private_file(&dest)?;
            std::io::copy(
                &mut std::io::Read::by_ref(&mut input).take(MAX_FILE_BYTES + 1),
                &mut out,
            )?;
        }
        let bytes = dest.metadata()?.len();
        ensure!(bytes <= MAX_FILE_BYTES, "backup file size limit exceeded");
        total = total.checked_add(bytes).context("backup size overflow")?;
        ensure!(total <= MAX_BYTES, "backup total size limit exceeded");
        let digest = hash(&mut File::open(&dest)?, &mut std::io::sink(), bytes)?;
        files.push(Entry {
            path: relative
                .to_str()
                .context("invalid path")?
                .replace('\\', "/"),
            bytes,
            sha256: digest,
        });
    }
    let manifest = Manifest {
        format: 1,
        created_ms: layeragent_protocol::now_ms(),
        schema_version: crate::SCHEMA_VERSION,
        files,
    };
    let encoded = serde_json::to_vec(&manifest)?;
    ensure!(
        encoded.len() as u64 <= MAX_MANIFEST,
        "manifest size limit exceeded"
    );
    // Temporary ciphertext is private and only installed after age authentication
    // has been finalized and fsync has completed.
    let mut ciphertext = tempfile::NamedTempFile::new_in(parent)?;
    let encryptor =
        age::Encryptor::with_recipients(std::iter::once(&public as &dyn age::Recipient))?;
    let mut writer = encryptor.wrap_output(ciphertext.as_file_mut())?;
    writer.write_all(MAGIC)?;
    writer.write_all(&(encoded.len() as u64).to_be_bytes())?;
    writer.write_all(&encoded)?;
    for entry in &manifest.files {
        let digest = hash(
            &mut File::open(stage.path().join(&entry.path))?,
            &mut writer,
            entry.bytes,
        )?;
        ensure!(digest == entry.sha256, "snapshot changed while archiving");
    }
    writer.finish()?;
    ciphertext.as_file().sync_all()?;
    ciphertext.persist_noclobber(output).map_err(|e| e.error)?;
    sync_dir(parent)?;
    Ok(manifest)
}
fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn read_manifest(reader: &mut dyn Read) -> Result<Manifest> {
    let mut magic = vec![0; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    ensure!(magic == MAGIC, "unsupported backup format");
    let mut size = [0; 8];
    reader.read_exact(&mut size)?;
    let size = u64::from_be_bytes(size);
    ensure!(size <= MAX_MANIFEST, "manifest size limit exceeded");
    let mut bytes = vec![0; size as usize];
    reader.read_exact(&mut bytes)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        manifest.format == 1 && manifest.schema_version <= crate::SCHEMA_VERSION,
        "unsupported backup version"
    );
    ensure!(
        manifest.files.len() <= MAX_FILES,
        "file count limit exceeded"
    );
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for entry in &manifest.files {
        ensure!(
            allowed(&entry.path)?,
            "backup entry outside state allowlist"
        );
        ensure!(paths.insert(&entry.path), "duplicate backup path");
        ensure!(
            entry.bytes <= MAX_FILE_BYTES,
            "backup file size limit exceeded"
        );
        ensure!(
            entry.sha256.len() == 64 && entry.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid digest"
        );
        total = total
            .checked_add(entry.bytes)
            .context("backup size overflow")?;
        ensure!(total <= MAX_BYTES, "backup total size limit exceeded");
    }
    Ok(manifest)
}
fn unpack(archive: &Path, key: &Path, destination: Option<&Path>) -> Result<Manifest> {
    reject_symlink_ancestors(archive)?;
    let archive_meta = archive.metadata()?;
    ensure!(
        archive_meta.is_file(),
        "backup archive must be a regular file"
    );
    ensure!(
        archive_meta.len() <= MAX_BYTES + MAX_MANIFEST + 16 * 1024 * 1024,
        "archive size limit exceeded"
    );
    // Generated archives have one short X25519 recipient stanza. Bound the
    // unauthenticated age header before handing it to the general parser.
    let mut header = Vec::new();
    File::open(archive)?.take(65_536).read_to_end(&mut header)?;
    ensure!(
        header.windows(5).any(|bytes| bytes == b"\n--- "),
        "age header size limit exceeded"
    );
    let identity = identity(key)?;
    let decryptor = age::Decryptor::new(File::open(archive)?)?;
    let mut reader = decryptor.decrypt(std::iter::once(&identity as &dyn age::Identity))?;
    let manifest = read_manifest(&mut reader)?;
    for entry in &manifest.files {
        let mut target: Box<dyn Write> = if let Some(base) = destination {
            let path = base.join(&entry.path);
            private_dir(path.parent().context("restore parent missing")?)?;
            Box::new(private_file(&path)?)
        } else {
            Box::new(std::io::sink())
        };
        let digest = hash(&mut reader, target.as_mut(), entry.bytes)?;
        ensure!(digest == entry.sha256, "backup checksum mismatch");
        target.flush()?;
    }
    // Read to authenticated EOF; no restored directory is published before this.
    let mut extra = [0];
    ensure!(
        reader.read(&mut extra)? == 0,
        "unexpected trailing backup payload"
    );
    Ok(manifest)
}
pub fn verify(archive: &Path, key: &Path) -> Result<Manifest> {
    unpack(archive, key, None)
}
/// Restore into a new, nonexistent home. Never merges with existing data and
/// never replaces an existing home, even if it is empty. Secrets must be reconnected.
pub fn restore(archive: &Path, key: &Path, destination: &Path) -> Result<Manifest> {
    reject_symlink_ancestors(destination)?;
    ensure!(
        !destination.exists(),
        "restore requires a new, nonexistent destination"
    );
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let stage = tempfile::tempdir_in(parent)?;
    let manifest = unpack(archive, key, Some(stage.path()))?;
    for entry in &manifest.files {
        let path = stage.path().join(&entry.path);
        if database_path(&entry.path)? {
            let db = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
            let check: String = db.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            ensure!(check == "ok", "restored database integrity check failed");
        }
        OpenOptions::new().write(true).open(path)?.sync_all()?;
    }
    let mut directories = BTreeSet::new();
    directories.insert(stage.path().to_path_buf());
    for entry in &manifest.files {
        let restored = stage.path().join(&entry.path);
        for directory in restored.parent().into_iter().flat_map(Path::ancestors) {
            if !directory.starts_with(stage.path()) {
                break;
            }
            directories.insert(directory.to_path_buf());
        }
    }
    for directory in directories.into_iter().rev() {
        sync_dir(&directory)?;
    }
    // Lock parent to prevent competing LayerAgent restores; refuse an existing
    // destination again immediately before rename. Home parent must be private
    // to the operator, not writable by another user.
    let guard_path = parent.join(".layeragent-restore.lock");
    ensure!(!guard_path.is_symlink(), "symlink restore lock refused");
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let guard = options.open(guard_path)?;
    guard
        .try_lock_exclusive()
        .context("another restore is active")?;
    ensure!(
        !destination.exists(),
        "restore destination appeared during verification"
    );
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        stage.path(),
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )?;
    #[cfg(target_os = "windows")]
    fs::rename(stage.path(), destination)?;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    anyhow::bail!("atomic non-overwriting restore is unsupported on this platform");
    sync_dir(parent)?;
    Ok(manifest)
}
/// Prune only archives created with LayerAgent's exact retention naming format.
/// An explicit directory and count prevent deleting arbitrary `.age` files.
pub fn retain(directory: &Path, keep: usize) -> Result<usize> {
    ensure!(keep > 0, "retention must keep at least one archive");
    reject_symlink_ancestors(directory)?;
    let mut archives = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name
            .strip_prefix("layeragent-backup-")
            .and_then(|s| s.strip_suffix(".age"))
            && id.len() == 20
            && id.bytes().all(|b| b.is_ascii_digit())
            && entry.file_type()?.is_file()
        {
            archives.push((name, entry.path()));
        }
    }
    archives.sort();
    let remove = archives.len().saturating_sub(keep);
    for (_, path) in archives.into_iter().take(remove) {
        fs::remove_file(path)?;
    }
    Ok(remove)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_dir() -> Result<tempfile::TempDir> {
        // macOS /var is a system symlink; fixture paths use its physical parent.
        Ok(tempfile::tempdir_in(std::env::temp_dir().canonicalize()?)?)
    }
    fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, String)> {
        let root = test_dir()?;
        let home = root.path().join("source");
        private_dir(&home)?;
        fs::write(
            home.join("config.toml"),
            "instructions = 'backup fixture'\n",
        )?;
        let dbpath = home.join("data/default/tenants/default/state.db");
        private_dir(dbpath.parent().context("parent")?)?;
        let db = Connection::open(dbpath)?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE test(value TEXT); INSERT INTO test VALUES('persisted');")?;
        let key = root.path().join("identity");
        let recipient = keygen(&key)?;
        Ok((root, home, key, recipient))
    }
    #[test]
    fn encrypted_roundtrip_verifies_sqlite_and_excludes_secrets() -> Result<()> {
        let (root, home, key, recipient) = fixture()?;
        fs::write(home.join("service.env"), "TOKEN=must-not-back-up")?;
        private_dir(&home.join("skills/example"))?;
        fs::write(home.join("skills/example/SKILL.md"), "test skill")?;
        fs::write(home.join("skills/example/.env"), "TOKEN=also-excluded")?;
        let archive = root.path().join("snapshot.age");
        let manifest = create(&home, &archive, &recipient)?;
        assert_eq!(manifest.files.len(), 3);
        assert!(!fs::read(&archive)?.windows(9).any(|w| w == b"persisted"));
        assert_eq!(verify(&archive, &key)?.files.len(), 3);
        let dest = root.path().join("restored");
        restore(&archive, &key, &dest)?;
        let db = Connection::open(dest.join("data/default/tenants/default/state.db"))?;
        assert_eq!(
            db.query_row("SELECT value FROM test", [], |r| r.get::<_, String>(0))?,
            "persisted"
        );
        assert!(!dest.join("service.env").exists());
        assert!(!dest.join("skills/example/.env").exists());
        assert!(restore(&archive, &key, &dest).is_err());
        assert!(create(&home, &archive, &recipient).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(dest.metadata()?.permissions().mode() & 0o777, 0o700);
            assert_eq!(
                dest.join("config.toml").metadata()?.permissions().mode() & 0o777,
                0o600
            );
        }
        Ok(())
    }
    #[test]
    fn active_runtime_wrong_key_tamper_and_truncation_fail_closed() -> Result<()> {
        let (root, home, key, recipient) = fixture()?;
        let archive = root.path().join("snapshot.age");
        let lease = runtime_lease(&home)?;
        assert!(create(&home, &archive, &recipient).is_err());
        drop(lease);
        create(&home, &archive, &recipient)?;
        let wrong = root.path().join("wrong");
        keygen(&wrong)?;
        assert!(verify(&archive, &wrong).is_err());
        let mut bytes = fs::read(&archive)?;
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&archive, &bytes)?;
        let dest = root.path().join("destination");
        assert!(restore(&archive, &key, &dest).is_err());
        assert!(!dest.exists());
        bytes.truncate(bytes.len() / 2);
        fs::write(&archive, bytes)?;
        assert!(verify(&archive, &key).is_err());
        fs::write(&archive, vec![b'a'; 65_537])?;
        assert!(verify(&archive, &key).is_err());
        assert!(verify(root.path(), &key).is_err());
        Ok(())
    }
    fn malicious(
        root: &Path,
        recipient: &str,
        manifest: &Manifest,
        payload: &[u8],
    ) -> Result<PathBuf> {
        let archive = root.join("malicious.age");
        let key: x25519::Recipient = recipient
            .parse()
            .map_err(|_| anyhow::anyhow!("recipient"))?;
        let mut encrypted =
            age::Encryptor::with_recipients(std::iter::once(&key as &dyn age::Recipient))?
                .wrap_output(File::create(&archive)?)?;
        let bytes = serde_json::to_vec(manifest)?;
        encrypted.write_all(MAGIC)?;
        encrypted.write_all(&(bytes.len() as u64).to_be_bytes())?;
        encrypted.write_all(&bytes)?;
        encrypted.write_all(payload)?;
        encrypted.finish()?;
        Ok(archive)
    }
    #[test]
    fn authenticated_malicious_manifest_and_extra_payload_rejected() -> Result<()> {
        let (root, _, key, recipient) = fixture()?;
        for path in [
            "../escape",
            "/absolute",
            "skills/x/../../escape",
            "skills/x/evil\\file",
            "runtime/token.json",
            "skills/x/.env",
            "profiles/C:evil.toml",
            "skills/x/CON",
            "skills/x/PRN.txt",
            "skills/x/a.",
        ] {
            let manifest = Manifest {
                format: 1,
                created_ms: 0,
                schema_version: 1,
                files: vec![Entry {
                    path: path.into(),
                    bytes: 0,
                    sha256: hex::encode(Sha256::digest([])),
                }],
            };
            let archive = malicious(root.path(), &recipient, &manifest, &[])?;
            assert!(verify(&archive, &key).is_err(), "accepted {path}");
        }
        let mut manifest = Manifest {
            format: 1,
            created_ms: 0,
            schema_version: 1,
            files: vec![],
        };
        let archive = malicious(root.path(), &recipient, &manifest, b"extra")?;
        assert!(verify(&archive, &key).is_err());
        manifest.files.push(Entry {
            path: "config.toml".into(),
            bytes: MAX_FILE_BYTES + 1,
            sha256: "a".repeat(64),
        });
        let archive = malicious(root.path(), &recipient, &manifest, b"")?;
        assert!(verify(&archive, &key).is_err());
        manifest.files[0].bytes = 0;
        manifest.files[0].sha256 = hex::encode(Sha256::digest([]));
        manifest.files.push(Entry {
            path: "config.toml".into(),
            bytes: 0,
            sha256: hex::encode(Sha256::digest([])),
        });
        let archive = malicious(root.path(), &recipient, &manifest, b"")?;
        assert!(verify(&archive, &key).is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_overpermissive_identity_rejected() -> Result<()> {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let (root, home, key, recipient) = fixture()?;
        private_dir(&home.join("skills/x"))?;
        symlink(&key, home.join("skills/x/key"))?;
        assert!(create(&home, &root.path().join("archive.age"), &recipient).is_err());
        fs::remove_file(home.join("skills/x/key"))?;
        let archive = root.path().join("archive.age");
        create(&home, &archive, &recipient)?;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o644))?;
        assert!(verify(&archive, &key).is_err());
        Ok(())
    }
    #[test]
    fn retention_preserves_unrelated_files() -> Result<()> {
        let dir = test_dir()?;
        for name in [
            "layeragent-backup-00000000000000000001.age",
            "layeragent-backup-00000000000000000002.age",
            "other.age",
            "layeragent-backup-not-a-timestamp.age",
        ] {
            fs::write(dir.path().join(name), "encrypted")?;
        }
        assert_eq!(retain(dir.path(), 1)?, 1);
        assert!(dir.path().join("other.age").exists());
        assert!(
            dir.path()
                .join("layeragent-backup-not-a-timestamp.age")
                .exists()
        );
        assert!(retain(dir.path(), 0).is_err());
        Ok(())
    }
}
