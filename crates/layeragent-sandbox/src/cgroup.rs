//! Optional cgroup v2 enforcement of memory, process count and CPU quota.
//!
//! Jobs are leaves below the current delegated group. With the shipped systemd
//! DelegateSubgroup=supervisor layout they are siblings of the daemon, beneath
//! the process-free service group. Requested missing controllers may be enabled
//! only at that delegated service boundary; other layouts must be preconfigured.
use anyhow::{Context, Result, bail, ensure};
use layeragent_config::Limits;
use std::{
    fs,
    path::{Path, PathBuf},
};

const ROOT: &str = "/sys/fs/cgroup";

pub(crate) struct Cgroup {
    path: PathBuf,
}

/// The cgroup v2 directory of the current process, if the unified hierarchy is mounted.
fn own_group() -> Result<PathBuf> {
    membership_path(&fs::read_to_string("/proc/self/cgroup")?)
}

fn membership_path(table: &str) -> Result<PathBuf> {
    let relative = table
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .context("cgroup v2 is not in use")?;
    ensure!(
        relative.starts_with('/') && !relative.split('/').any(|p| matches!(p, "." | "..")),
        "invalid cgroup membership path"
    );
    Ok(Path::new(ROOT).join(relative.trim_start_matches('/')))
}

/// Match only the documented systemd service layout. Do not walk arbitrary
/// ancestors: a writable cgroup is not permission to manage the host hierarchy.
fn delegation_parent(own: &Path) -> (PathBuf, bool) {
    if own.file_name().is_some_and(|n| n == "supervisor")
        && let Some(parent) = own.parent()
        && parent
            .file_name()
            .is_some_and(|n| n.to_string_lossy().ends_with(".service"))
    {
        return (parent.into(), true);
    }
    (own.into(), false)
}

fn probe_parent(parent: &Path) -> Result<Vec<String>> {
    let controllers = fs::read_to_string(parent.join("cgroup.subtree_control"))
        .context("cgroup is not delegated to this user")?;
    if fs::OpenOptions::new()
        .write(true)
        .open(parent.join("cgroup.procs"))
        .is_err()
    {
        bail!("cgroup {} is not writable", parent.display());
    }
    Ok(controllers.split_whitespace().map(str::to_owned).collect())
}

/// Read-only diagnostic: currently enabled child controllers, without changing host state.
pub(crate) fn probe(limits: &Limits) -> Result<Vec<String>> {
    let (parent, _) = delegation_parent(&own_group()?);
    let enabled = probe_parent(&parent)?;
    verify_requested_controllers(&enabled, limits)?;
    Ok(enabled)
}

fn verify_requested_controllers(enabled: &[String], limits: &Limits) -> Result<()> {
    if limits.cgroup {
        for name in requested_controllers(limits) {
            ensure!(
                enabled.iter().any(|n| n == name),
                "requested cgroup controller {name} is not enabled for child groups"
            );
        }
    }
    Ok(())
}

fn requested_controllers(limits: &Limits) -> Vec<&'static str> {
    [
        ("memory", limits.memory_bytes.is_some()),
        ("pids", limits.max_pids.is_some()),
        ("cpu", limits.cpu_percent.is_some()),
    ]
    .into_iter()
    .filter_map(|(name, wanted)| wanted.then_some(name))
    .collect()
}

fn enable_command(enabled: &[String], available: &[String], requested: &[&str]) -> Result<String> {
    let mut missing = Vec::new();
    for name in requested {
        if enabled.iter().any(|n| n == name) {
            continue;
        }
        ensure!(
            available.iter().any(|n| n == name),
            "cgroup controller {name} is not delegated to this service"
        );
        missing.push(format!("+{name}"));
    }
    // '+' changes only these bits, preserving every previously enabled controller.
    Ok(missing.join(" "))
}

fn prepare_parent(parent: &Path, service_layout: bool, limits: &Limits) -> Result<()> {
    let enabled = probe_parent(parent)?;
    let requested = requested_controllers(limits);
    if requested
        .iter()
        .all(|name| enabled.iter().any(|n| n == name))
    {
        return Ok(());
    }
    ensure!(
        service_layout,
        "requested cgroup controllers are not enabled for children; run with the delegated service template"
    );
    ensure!(
        fs::read_to_string(parent.join("cgroup.procs"))?
            .trim()
            .is_empty(),
        "delegated service parent contains processes; cannot enable domain controllers"
    );
    let available: Vec<String> = fs::read_to_string(parent.join("cgroup.controllers"))?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let command = enable_command(&enabled, &available, &requested)?;
    if !command.is_empty() {
        fs::write(parent.join("cgroup.subtree_control"), command)
            .context("enabling delegated service child controllers")?;
    }
    let now = probe_parent(parent)?;
    ensure!(
        requested.iter().all(|name| now.iter().any(|n| n == name)),
        "requested cgroup controllers did not become active"
    );
    Ok(())
}

impl Cgroup {
    pub(crate) fn create(limits: &Limits) -> Result<Self> {
        let (parent, service_layout) = delegation_parent(&own_group()?);
        prepare_parent(&parent, service_layout, limits)?;
        let path = parent.join(format!("layeragent-{}", layeragent_protocol::new_id()));
        fs::create_dir(&path).context("creating cgroup leaf")?;
        let group = Self { path };
        if let Some(bytes) = limits.memory_bytes {
            group.write("memory.max", &bytes.to_string())?;
            // No swap escape hatch for the workload.
            if group.path.join("memory.swap.max").exists() {
                group.write("memory.swap.max", "0")?;
            }
        }
        if let Some(n) = limits.max_pids {
            group.write("pids.max", &n.to_string())?;
        }
        if let Some(percent) = limits.cpu_percent {
            group.write("cpu.max", &format!("{} 100000", u64::from(percent) * 1000))?;
        }
        Ok(group)
    }

    fn write(&self, file: &str, value: &str) -> Result<()> {
        fs::write(self.path.join(file), value).with_context(|| format!("writing {file}"))
    }

    /// Enters the group in the child before exec, so no workload can fork outside it.
    pub(crate) fn attach_before_exec(&self, command: &mut tokio::process::Command) -> Result<()> {
        use std::os::fd::AsRawFd;
        let procs = fs::OpenOptions::new()
            .write(true)
            .open(self.path.join("cgroup.procs"))?;
        // SAFETY: only async-signal-safe write is called between fork and exec. The
        // descriptor is opened by the parent and owned by the pre_exec closure.
        unsafe {
            command.pre_exec(move || {
                if libc::write(procs.as_raw_fd(), b"0".as_ptr().cast(), 1) != 1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // cgroup.kill terminates every member, including double-forked descendants.
        let _ = self.write("cgroup.kill", "1");
        for _ in 0..20 {
            if fs::remove_dir(&self.path).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supervisor_jobs_are_siblings_only_within_a_systemd_service() {
        let service = Path::new("/sys/fs/cgroup/user.slice/layeragent.service");
        assert_eq!(
            delegation_parent(&service.join("supervisor")),
            (service.into(), true)
        );
        for path in [
            "/sys/fs/cgroup/supervisor",
            "/sys/fs/cgroup/other.scope/supervisor",
            "/sys/fs/cgroup/layeragent.service/worker",
        ] {
            assert_eq!(
                delegation_parent(Path::new(path)),
                (PathBuf::from(path), false)
            );
        }
        assert_eq!(
            membership_path("0::/user.slice/layeragent.service/supervisor\n").unwrap(),
            service.join("supervisor")
        );
        assert!(membership_path("0::/../other\n").is_err());
    }

    #[test]
    fn controller_updates_preserve_existing_settings_and_require_delegation() {
        let enabled = vec!["cpu".into(), "io".into()];
        let available = vec!["memory".into(), "cpu".into(), "pids".into(), "io".into()];
        assert_eq!(
            enable_command(&enabled, &available, &["cpu", "memory", "pids"]).unwrap(),
            "+memory +pids"
        );
        assert!(enable_command(&enabled, &available, &["unknown"]).is_err());
        assert_eq!(enable_command(&enabled, &available, &["cpu"]).unwrap(), "");
    }

    #[test]
    fn occupied_service_parent_and_ordinary_groups_fail_before_mutation() {
        let group = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("cgroup.procs", "1234\n"),
            ("cgroup.subtree_control", ""),
            ("cgroup.controllers", "memory pids cpu"),
        ] {
            fs::write(group.path().join(name), text).unwrap();
        }
        let limits = Limits {
            memory_bytes: Some(1024),
            ..Default::default()
        };
        assert!(prepare_parent(group.path(), true, &limits).is_err());
        assert_eq!(
            fs::read_to_string(group.path().join("cgroup.subtree_control")).unwrap(),
            ""
        );
        fs::write(group.path().join("cgroup.procs"), "").unwrap();
        assert!(prepare_parent(group.path(), false, &limits).is_err());
        assert_eq!(
            fs::read_to_string(group.path().join("cgroup.subtree_control")).unwrap(),
            ""
        );
    }
    #[test]
    fn doctor_requires_every_configured_controller_not_merely_any_controller() {
        let limits = Limits {
            cgroup: true,
            memory_bytes: Some(1 << 20),
            max_pids: Some(16),
            cpu_percent: Some(50),
            ..Default::default()
        };
        assert!(verify_requested_controllers(&["pids".into()], &limits).is_err());
        assert!(verify_requested_controllers(&["pids".into(), "memory".into()], &limits).is_err());
        assert!(
            verify_requested_controllers(&["pids".into(), "memory".into(), "cpu".into()], &limits)
                .is_ok()
        );
        let optional = Limits {
            cgroup: false,
            ..limits
        };
        assert!(verify_requested_controllers(&[], &optional).is_ok());
    }
}
