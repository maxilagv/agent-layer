//! Descriptor-relative Linux file broker. No caller-controlled ambient paths.
use crate::{McpGrant, digest};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use layeragent_protocol::Scope;
use serde_json::{Value, json};
use std::{
    ffi::CString,
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
};

const MAX_FILE: u64 = 16 * 1024 * 1024;
/// Maximum bytes returned by one MCP resource read.
pub const RESOURCE_READ_LIMIT: usize = 64 * 1024;
/// Maximum depth and breadth of a resource catalog.
const RESOURCE_DEPTH: usize = 6;
const RESOURCE_ENTRIES: usize = 2000;
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn open_at(root: RawFd, path: &str, flags: i32, mode: u32) -> Result<File> {
    let path = CString::new(path)?;
    // BENEATH | NO_MAGICLINKS | NO_SYMLINKS | NO_XDEV. Never weaken on ENOSYS.
    let how = OpenHow {
        flags: (flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: u64::from(mode),
        resolve: 0x08 | 0x02 | 0x04 | 0x01,
    };
    // SAFETY: valid C string, stack-owned ABI structure and descriptor; return fd is owned.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            root,
            path.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    ensure!(
        fd >= 0,
        "safe path open rejected: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(fd as RawFd) })
}
fn regular(file: &File) -> Result<()> {
    let m = file.metadata()?;
    ensure!(
        m.is_file() && m.nlink() == 1 && m.size() <= MAX_FILE,
        "regular unlinked-alias-free bounded file required"
    );
    Ok(())
}
fn read(file: &mut File) -> Result<Vec<u8>> {
    regular(file)?;
    let mut data = Vec::new();
    file.take(MAX_FILE + 1).read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= MAX_FILE, "file exceeds limit");
    Ok(data)
}
/// All broker mutations are serialized with a descriptor lock. Uncooperative
/// editors can still race POSIX replacement; expected hashes detect observed
/// conflicts, not an impossible cross-application filesystem CAS guarantee.
pub struct FileBroker {
    root: File,
    grant: McpGrant,
    root_identity: (u64, u64),
    local_lock: std::sync::Mutex<()>,
    authority_file: Option<(std::path::PathBuf, String)>,
}
impl FileBroker {
    pub fn open(grant: McpGrant) -> Result<Self> {
        grant.validate()?;
        let root = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&grant.root)?;
        let m = root.metadata()?;
        ensure!(m.is_dir(), "project root must be a directory");
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        ensure!(
            unsafe { libc::fstatfs(root.as_raw_fd(), filesystem.as_mut_ptr()) } == 0,
            "cannot validate project filesystem"
        );
        let kind = unsafe { filesystem.assume_init() }.f_type as u64;
        ensure!(
            ![
                0x9fa0, 0x62656572, 0x1cd1, 0x63677270, 0x27e0eb, 0x74726163, 0x64626720,
                0xcafe4a11, 0xde5e81e4
            ]
            .contains(&kind),
            "virtual system filesystem cannot be used as a project"
        );
        // Exercise the actual resolver before accepting this host.
        open_at(root.as_raw_fd(), ".", libc::O_PATH | libc::O_DIRECTORY, 0)?;
        let broker = Self {
            root,
            grant,
            root_identity: (m.dev(), m.ino()),
            local_lock: std::sync::Mutex::new(()),
            authority_file: None,
        };
        broker.confined(false, || Ok(()))?;
        Ok(broker)
    }
    pub fn root(&self) -> &Path {
        &self.grant.root
    }
    pub fn grant(&self) -> &McpGrant {
        &self.grant
    }
    pub fn open_reviewed_file(path: &Path) -> Result<Self> {
        let raw = reviewed_bytes(path)?;
        let grant: McpGrant = serde_json::from_slice(&raw)?;
        let mut broker = Self::open(grant)?;
        broker.authority_file = Some((std::env::current_dir()?.join(path), digest(&raw)));
        Ok(broker)
    }
    pub(crate) fn check_source(&self) -> Result<()> {
        if let Some((path, expected)) = &self.authority_file {
            ensure!(
                digest(&reviewed_bytes(path)?) == *expected,
                "MCP grant file changed; owner review and restart required"
            );
        }
        Ok(())
    }
    pub fn read_bytes_checked(
        &self,
        scope: &Scope,
        path: &str,
        check: impl Fn() -> Result<()> + Sync,
    ) -> Result<Vec<u8>> {
        self.grant
            .authorize(scope, "read_file", &json!({"path":path}))?;
        let current = std::fs::symlink_metadata(&self.grant.root)?;
        ensure!(
            !current.file_type().is_symlink()
                && (current.dev(), current.ino()) == self.root_identity,
            "project root changed"
        );
        self.confined(false, || {
            self.check_source()?;
            check()?;
            read(&mut open_at(
                self.root.as_raw_fd(),
                path,
                libc::O_RDONLY | libc::O_NONBLOCK,
                0,
            )?)
        })
    }
    /// Bounded catalog of readable project files for MCP `resources/list`.
    ///
    /// The catalog is derived from the same read destinations as the file
    /// tools. It exposes names and sizes only; content requires an explicit
    /// read.
    pub fn resource_catalog(
        &self,
        scope: &Scope,
        check: impl Fn() -> Result<()> + Sync,
    ) -> Result<Vec<Value>> {
        ensure!(&self.grant.scope == scope, "MCP identity outside grant");
        let destinations = self.grant.read_destinations()?;
        self.check_source()?;
        let current = std::fs::symlink_metadata(&self.grant.root)?;
        ensure!(
            !current.file_type().is_symlink()
                && (current.dev(), current.ino()) == self.root_identity,
            "project root changed"
        );
        let _local = self
            .local_lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("project broker busy"))?;
        self.root
            .try_lock_exclusive()
            .context("project broker busy")?;
        let verify = || {
            self.check_source()?;
            check()
        };
        let result = self.confined(false, || {
            verify()?;
            let mut out: Vec<Value> = Vec::new();
            for destination in &destinations {
                let directory = open_at(
                    self.root.as_raw_fd(),
                    destination,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?;
                self.walk_resources(&directory, destination, 0, &mut out)?;
            }
            out.sort_by(|a, b| {
                a["path"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(b["path"].as_str().unwrap_or_default())
            });
            out.truncate(RESOURCE_ENTRIES);
            Ok(out)
        });
        FileExt::unlock(&self.root)?;
        result
    }
    fn walk_resources(
        &self,
        directory: &File,
        prefix: &str,
        depth: usize,
        out: &mut Vec<Value>,
    ) -> Result<()> {
        if depth > RESOURCE_DEPTH || out.len() >= RESOURCE_ENTRIES {
            return Ok(());
        }
        let mut names = Vec::new();
        for (visited, entry) in
            std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?.enumerate()
        {
            ensure!(visited < 1000, "directory listing exceeds limit");
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if crate::validate_path(&name, false).is_err() {
                continue;
            }
            names.push(name);
        }
        names.sort();
        for name in names {
            if out.len() >= RESOURCE_ENTRIES {
                break;
            }
            let Ok(file) = open_at(directory.as_raw_fd(), &name, libc::O_PATH, 0) else {
                continue;
            };
            let metadata = file.metadata()?;
            let relative = if prefix == "." {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if metadata.is_dir() {
                self.walk_resources(&file, &relative, depth + 1, out)?;
            } else if metadata.is_file() && metadata.nlink() == 1 {
                out.push(json!({"path": relative, "bytes": metadata.size()}));
            }
        }
        Ok(())
    }
    /// Reads one resource as bounded UTF-8.
    ///
    /// A file larger than the limit is truncated and reported as truncated;
    /// the complete content is available through the existing paginated read.
    pub fn read_resource(
        &self,
        scope: &Scope,
        path: &str,
        check: impl Fn() -> Result<()> + Sync,
    ) -> Result<Value> {
        self.grant.authorize_read_path(scope, path)?;
        self.check_source()?;
        let current = std::fs::symlink_metadata(&self.grant.root)?;
        ensure!(
            !current.file_type().is_symlink()
                && (current.dev(), current.ino()) == self.root_identity,
            "project root changed"
        );
        let _local = self
            .local_lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("project broker busy"))?;
        self.root
            .try_lock_exclusive()
            .context("project broker busy")?;
        let verify = || {
            self.check_source()?;
            check()
        };
        let result = self.confined(false, || {
            verify()?;
            let file = open_at(
                self.root.as_raw_fd(),
                path,
                libc::O_RDONLY | libc::O_NONBLOCK,
                0,
            )?;
            regular(&file)?;
            let mut data = Vec::new();
            file.take(RESOURCE_READ_LIMIT as u64 + 1)
                .read_to_end(&mut data)?;
            let mut truncated = data.len() > RESOURCE_READ_LIMIT;
            if truncated {
                data.truncate(RESOURCE_READ_LIMIT);
            }
            let text = match std::str::from_utf8(&data) {
                Ok(text) => text.to_string(),
                Err(error) if error.error_len().is_none() => {
                    // A character straddles the limit: keep the complete prefix.
                    truncated = true;
                    String::from_utf8_lossy(&data[..error.valid_up_to()]).into_owned()
                }
                Err(_) => anyhow::bail!("resource is not UTF-8 text"),
            };
            Ok(json!({
                "path": path,
                "text": text,
                "bytes": data.len(),
                "truncated": truncated,
                "content_sha256": digest(&data),
            }))
        });
        FileExt::unlock(&self.root)?;
        result
    }
    pub fn invoke(&self, scope: &Scope, tool: &str, args: &Value) -> Result<Value> {
        self.invoke_checked(scope, tool, args, || Ok(()))
    }
    pub fn invoke_checked(
        &self,
        scope: &Scope,
        tool: &str,
        args: &Value,
        check: impl Fn() -> Result<()> + Sync,
    ) -> Result<Value> {
        self.grant.authorize(scope, tool, args)?;
        self.check_source()?;
        let current = std::fs::symlink_metadata(&self.grant.root)?;
        ensure!(
            !current.file_type().is_symlink()
                && (current.dev(), current.ino()) == self.root_identity,
            "project root changed; reauthorization required"
        );
        let _local = self
            .local_lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("project broker busy"))?;
        self.root
            .try_lock_exclusive()
            .context("project broker busy")?;
        let mutation = crate::operation(tool)? != crate::Operation::Read;
        let verify = || {
            self.check_source()?;
            check()
        };
        let result = self.confined(mutation, || {
            verify().and_then(|_| self.locked(tool, args, &verify))
        });
        FileExt::unlock(&self.root)?;
        result
    }
    /// Fresh threads prevent irreversible Landlock rules from contaminating a
    /// shared async/blocking pool. The trusted broker keeps read access needed
    /// for its local authority checks; input paths still go through openat2.
    /// Kernel write checks also cover a parent moved after path resolution.
    fn confined<T: Send>(
        &self,
        mutation: bool,
        work: impl FnOnce() -> Result<T> + Send,
    ) -> Result<T> {
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    use landlock::{
                        ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr,
                        RulesetCreatedAttr, RulesetStatus,
                    };
                    let abi = ABI::V3;
                    let mut rules = Ruleset::default()
                        .handle_access(AccessFs::from_all(abi))?
                        .create()?
                        .add_rule(PathBeneath::new(
                            PathFd::new("/")?,
                            AccessFs::from_read(abi),
                        ))?;
                    if mutation {
                        rules = rules
                            .add_rule(PathBeneath::new(&self.root, AccessFs::from_all(abi)))?;
                    }
                    ensure!(
                        rules.restrict_self()?.ruleset == RulesetStatus::FullyEnforced,
                        "broker requires fully enforced Landlock ABI 3"
                    );
                    work()
                })
                .join()
                .map_err(|_| anyhow::anyhow!("confined broker worker failed"))?
        })
    }
    fn locked(&self, tool: &str, args: &Value, check: &impl Fn() -> Result<()>) -> Result<Value> {
        let path = args["path"].as_str().context("path required")?;
        if tool == "list_files" {
            let directory = open_at(
                self.root.as_raw_fd(),
                path,
                libc::O_RDONLY | libc::O_DIRECTORY,
                0,
            )?;
            let mut entries = Vec::new();
            let mut directories = Vec::new();
            for (visited, entry) in
                std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))?.enumerate()
            {
                ensure!(visited < 1000, "directory listing exceeds limit");
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                if crate::validate_path(&name, false).is_err() {
                    continue;
                }
                if let Ok(file) = open_at(directory.as_raw_fd(), &name, libc::O_PATH, 0) {
                    let metadata = file.metadata()?;
                    if metadata.is_dir() {
                        directories.push(name.clone());
                    }
                    if metadata.is_dir() || (metadata.is_file() && metadata.nlink() == 1) {
                        entries.push(name);
                    }
                }
                ensure!(entries.len() <= 1000, "directory listing exceeds limit");
            }
            entries.sort();
            directories.sort();
            return Ok(json!({"entries":entries,"directories":directories}));
        }
        if tool == "read_file" {
            let mut file = open_at(
                self.root.as_raw_fd(),
                path,
                libc::O_RDONLY | libc::O_NONBLOCK,
                0,
            )?;
            let data = read(&mut file)?;
            let text = std::str::from_utf8(&data).context("UTF-8 file required")?;
            let offset = args["offset"].as_u64().unwrap_or(0) as usize;
            let limit = args["limit"].as_u64().unwrap_or(16384) as usize;
            let characters = text.chars().count();
            let slice = text.chars().skip(offset).take(limit).collect::<String>();
            return Ok(
                json!({"text":slice,"sha256":digest(&data),"offset":offset,"characters":characters,"truncated":offset.saturating_add(limit)<characters}),
            );
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or((".", path));
        let parent = open_at(
            self.root.as_raw_fd(),
            parent,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        let cname = CString::new(name)?;
        // Distinguish absence from resolver/access errors, including symlinks.
        let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
        let status = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                cname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        let exists = if status == 0 {
            true
        } else {
            ensure!(
                std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT),
                "destination cannot be inspected"
            );
            false
        };
        let old = if exists {
            read(&mut open_at(
                parent.as_raw_fd(),
                name,
                libc::O_RDONLY | libc::O_NONBLOCK,
                0,
            )?)?
        } else {
            Vec::new()
        };
        if exists || tool != "write_file" {
            ensure!(
                exists && args["expected_sha256"].as_str() == Some(digest(&old).as_str()),
                "file changed or expected hash missing"
            );
        } else {
            ensure!(
                args.get("expected_sha256").is_none(),
                "expected file disappeared"
            );
        }
        if tool == "delete_file" {
            check()?;
            ensure!(
                unsafe { libc::unlinkat(parent.as_raw_fd(), cname.as_ptr(), 0) } == 0,
                "delete failed"
            );
            parent.sync_all()?;
            return Ok(json!({"deleted":path}));
        }
        let text = if tool == "edit_file" {
            let text = std::str::from_utf8(&old)?;
            let needle = args["old"].as_str().context("old required")?;
            ensure!(
                text.matches(needle).count() == 1,
                "edit fragment must match exactly once"
            );
            text.replacen(needle, args["new"].as_str().context("new required")?, 1)
        } else {
            args["content"]
                .as_str()
                .context("content required")?
                .to_owned()
        };
        ensure!(text.len() as u64 <= MAX_FILE, "result exceeds file limit");
        let temp = format!(".layeragent-write-{}", uuid::Uuid::new_v4());
        let ctemp = CString::new(temp.clone())?;
        let outcome = (|| -> Result<()> {
            let mut output = open_at(
                parent.as_raw_fd(),
                &temp,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )?;
            output.write_all(text.as_bytes())?;
            if exists {
                let metadata = open_at(parent.as_raw_fd(), name, libc::O_PATH, 0)?.metadata()?;
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(std::fs::Permissions::from_mode(metadata.mode() & 0o777))?;
            }
            output.sync_all()?;
            if exists {
                let mut current = open_at(
                    parent.as_raw_fd(),
                    name,
                    libc::O_RDONLY | libc::O_NONBLOCK,
                    0,
                )?;
                ensure!(
                    digest(&read(&mut current)?) == digest(&old),
                    "file changed during editing"
                );
            }
            let flags = if exists { 0 } else { libc::RENAME_NOREPLACE };
            check()?;
            ensure!(
                unsafe {
                    libc::syscall(
                        libc::SYS_renameat2,
                        parent.as_raw_fd(),
                        ctemp.as_ptr(),
                        parent.as_raw_fd(),
                        cname.as_ptr(),
                        flags,
                    )
                } == 0,
                "atomic replacement failed"
            );
            parent.sync_all()?;
            Ok(())
        })();
        if outcome.is_err() {
            unsafe { libc::unlinkat(parent.as_raw_fd(), ctemp.as_ptr(), 0) };
        }
        outcome?;
        Ok(json!({"path":path,"sha256":digest(text.as_bytes())}))
    }
}
use std::os::unix::fs::OpenOptionsExt;
fn reviewed_bytes(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.len() <= 65536
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "MCP grant must be a bounded private file owned by the operator"
    );
    let mut raw = Vec::new();
    file.take(65537).read_to_end(&mut raw)?;
    ensure!(raw.len() <= 65536, "MCP grant exceeds limit");
    Ok(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Operation, ToolGrant, catalog_hash};
    use std::collections::{BTreeMap, BTreeSet};
    fn broker(root: &Path, tools: &[&str]) -> FileBroker {
        FileBroker::open(McpGrant {
            scope: Scope::local(),
            project_id: "test".into(),
            root: root.into(),
            revision: 1,
            catalog_sha256: catalog_hash(),
            tools: tools
                .iter()
                .map(|t| {
                    (
                        t.to_string(),
                        ToolGrant {
                            operations: BTreeSet::from([crate::operation(t).unwrap()]),
                            destinations: BTreeSet::from([".".into()]),
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        })
        .unwrap()
    }
    #[test]
    fn real_effects_require_separate_permissions() {
        let d = tempfile::tempdir().unwrap();
        let read = broker(d.path(), &["read_file"]);
        assert!(
            read.invoke(
                &Scope::local(),
                "write_file",
                &json!({"path":"a","content":"no"})
            )
            .is_err()
        );
        assert!(!d.path().join("a").exists());
        let write = broker(d.path(), &["write_file"]);
        let result = write
            .invoke(
                &Scope::local(),
                "write_file",
                &json!({"path":"a","content":"yes"}),
            )
            .unwrap();
        assert!(
            write
                .invoke(
                    &Scope::local(),
                    "delete_file",
                    &json!({"path":"a","expected_sha256":result["sha256"]})
                )
                .is_err()
        );
        assert!(
            write
                .invoke(
                    &Scope::local(),
                    "write_file",
                    &json!({"path":"a","content":"overwrite"})
                )
                .is_err()
        );
        broker(d.path(), &["delete_file"])
            .invoke(
                &Scope::local(),
                "delete_file",
                &json!({"path":"a","expected_sha256":result["sha256"]}),
            )
            .unwrap();
        assert!(!d.path().join("a").exists());
        let _ = Operation::Send;
    }
    #[test]
    fn aliases_special_files_and_root_replacement_fail_closed() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        std::fs::hard_link(outside.path().join("secret"), d.path().join("hard")).unwrap();
        let b = broker(d.path(), &["read_file"]);
        for path in ["link/secret", "hard", "../secret", ".env"] {
            assert!(
                b.invoke(&Scope::local(), "read_file", &json!({"path":path}))
                    .is_err()
            );
        }
        let moved = d.path().with_extension("moved");
        std::fs::rename(d.path(), &moved).unwrap();
        std::fs::create_dir(d.path()).unwrap();
        assert!(
            b.invoke(&Scope::local(), "read_file", &json!({"path":"a"}))
                .is_err()
        );
        std::fs::remove_dir_all(moved).unwrap();
    }
    #[test]
    fn kernel_checks_hold_even_if_broker_code_attempts_a_forbidden_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let broker = broker(root.path(), &["write_file"]);
        broker
            .confined(false, || {
                assert!(std::fs::write(root.path().join("no"), "no").is_err());
                Ok(())
            })
            .unwrap();
        broker
            .confined(true, || {
                assert!(std::fs::write(outside.path().join("no"), "no").is_err());
                std::fs::write(root.path().join("yes"), "yes")?;
                Ok(())
            })
            .unwrap();
        assert!(!outside.path().join("no").exists());
        assert!(root.path().join("yes").exists());
    }
    #[test]
    fn parent_moved_after_resolution_cannot_receive_an_outside_write() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("docs")).unwrap();
        let broker = broker(root.path(), &["write_file"]);
        let parent = open_at(
            broker.root.as_raw_fd(),
            "docs",
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )
        .unwrap();
        std::fs::rename(root.path().join("docs"), outside.path().join("moved")).unwrap();
        broker
            .confined(true, || {
                assert!(
                    open_at(
                        parent.as_raw_fd(),
                        "escape",
                        libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY,
                        0o600
                    )
                    .is_err()
                );
                Ok(())
            })
            .unwrap();
        assert!(!outside.path().join("moved/escape").exists());
    }
    #[test]
    fn reads_are_paged_but_keep_the_full_file_hash() {
        let root = tempfile::tempdir().unwrap();
        let text = "🦀".repeat(100000);
        std::fs::write(root.path().join("large"), &text).unwrap();
        let broker = broker(root.path(), &["read_file"]);
        let first = broker
            .invoke(&Scope::local(), "read_file", &json!({"path":"large"}))
            .unwrap();
        assert_eq!(first["text"].as_str().unwrap().chars().count(), 16384);
        assert_eq!(first["sha256"], digest(text.as_bytes()));
        assert_eq!(first["truncated"], true);
        let next = broker
            .invoke(
                &Scope::local(),
                "read_file",
                &json!({"path":"large","offset":16384,"limit":65536}),
            )
            .unwrap();
        assert_eq!(next["text"].as_str().unwrap().chars().count(), 65536);
    }
    #[test]
    fn changing_a_reviewed_mcp_grant_revokes_existing_broker_authority() {
        let root = tempfile::tempdir().unwrap();
        let control = tempfile::tempdir().unwrap();
        let original = broker(root.path(), &["write_file"]);
        let mut grant = original.grant().clone();
        let path = control.path().join("grant.json");
        std::fs::write(&path, serde_json::to_vec(&grant).unwrap()).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let reviewed = FileBroker::open_reviewed_file(&path).unwrap();
        reviewed
            .invoke(
                &Scope::local(),
                "write_file",
                &json!({"path":"before","content":"yes"}),
            )
            .unwrap();
        grant.tools.clear();
        std::fs::write(&path, serde_json::to_vec(&grant).unwrap()).unwrap();
        assert!(
            reviewed
                .invoke(
                    &Scope::local(),
                    "write_file",
                    &json!({"path":"after","content":"no"})
                )
                .is_err()
        );
        assert!(!root.path().join("after").exists());
    }
    #[test]
    fn system_directories_are_not_project_capabilities() {
        let root = tempfile::tempdir().unwrap();
        let base = broker(root.path(), &["read_file"]);
        for path in [
            "/proc",
            "/proc/self",
            "/sys",
            "/dev",
            "/etc",
            "/home",
            "/tmp",
            "/",
        ] {
            let mut grant = base.grant().clone();
            grant.root = path.into();
            assert!(FileBroker::open(grant).is_err());
        }
    }
}
