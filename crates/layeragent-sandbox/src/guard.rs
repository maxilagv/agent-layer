//! Platform-independent decisions about what a sandbox must hide or freeze.
#![cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
use anyhow::{Result, ensure};
use layeragent_config::Policy;
use std::path::{Path, PathBuf};

const INVENTORY_LIMIT: usize = 200_000;
const DEFAULT_PROTECTED: [&str; 3] = [".git", ".agents", ".layeragent"];
const SECRET_NAMES: [&str; 8] = [
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
];

#[derive(Clone, Debug)]
pub(crate) struct RootSpec {
    pub path: PathBuf,
    pub write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GuardKind {
    /// Must be unreadable and unwritable.
    Secret,
    /// Must stay readable but immutable.
    Protected,
}

#[derive(Clone, Debug)]
pub(crate) struct Guard {
    pub path: PathBuf,
    pub kind: GuardKind,
    pub dir: bool,
}

fn canonical_root(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    ensure!(
        path.is_dir() && path.parent().is_some(),
        "sandbox roots must be existing non-root directories: {}",
        path.display()
    );
    ensure!(
        !["/proc", "/sys", "/dev", "/run"]
            .iter()
            .any(|special| path.starts_with(special)),
        "sandbox root {} is a special filesystem",
        path.display()
    );
    Ok(path)
}

/// Workspace plus extra roots, read-only ones first so nested writable roots win.
pub(crate) fn resolve_roots(workspace: &Path, policy: &Policy) -> Result<Vec<RootSpec>> {
    let mut roots = vec![RootSpec {
        path: canonical_root(workspace)?,
        write: policy.write,
    }];
    for path in &policy.read_roots {
        roots.push(RootSpec {
            path: canonical_root(path)?,
            write: false,
        });
    }
    for path in &policy.write_roots {
        roots.push(RootSpec {
            path: canonical_root(path)?,
            write: true,
        });
    }
    roots.sort_by_key(|root| root.write);
    Ok(roots)
}

fn is_secret(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.") || SECRET_NAMES.contains(&name)
}

/// A protected entry is either a bare name matching anywhere, or a path relative to its root.
fn is_protected(name: &str, relative: &Path, extra: &[String]) -> bool {
    DEFAULT_PROTECTED.contains(&name)
        || extra.iter().any(|entry| {
            if entry.contains('/') {
                relative == Path::new(entry.trim_matches('/'))
            } else {
                entry == name
            }
        })
}

/// Finds secrets and protected metadata inside every root. Symlinked guards are refused
/// because they could redirect a mask onto an unrelated target.
pub(crate) fn scan_guards(roots: &[RootSpec], policy: &Policy) -> Result<Vec<Guard>> {
    let mut budget = INVENTORY_LIMIT;
    let mut guards = Vec::new();
    for root in roots {
        let mut inventory = walkdir::WalkDir::new(&root.path)
            .follow_links(false)
            .into_iter();
        while let Some(entry) = inventory.next() {
            let entry = entry?;
            if entry.depth() == 0 {
                continue;
            }
            ensure!(budget > 0, "sandbox inventory limit exceeded");
            budget -= 1;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = entry
                .path()
                .strip_prefix(&root.path)
                .unwrap_or(entry.path());
            let kind = if is_secret(&name) {
                GuardKind::Secret
            } else if is_protected(&name, relative, &policy.protected_paths) {
                GuardKind::Protected
            } else {
                continue;
            };
            ensure!(!entry.path_is_symlink(), "guarded path cannot be a symlink");
            let dir = entry.file_type().is_dir();
            guards.push(Guard {
                path: entry.path().to_path_buf(),
                kind,
                dir,
            });
            if dir {
                inventory.skip_current_dir();
            }
        }
    }
    Ok(guards)
}
