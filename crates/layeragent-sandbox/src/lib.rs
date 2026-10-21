#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use layeragent_config::Policy;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;
mod browser;
#[cfg(target_os = "linux")]
mod cgroup;
mod guard;
mod inner;
#[cfg(target_os = "linux")]
mod plan;
mod proxy;
mod pty;
mod seatbelt;
mod windows;
pub use browser::{BrowserInnerSpec, browser_command, browser_inner};
#[cfg(not(windows))]
pub use inner::inner;
pub use layeragent_config::Limits;
pub use proxy::{NetworkDecider, NetworkDecision, NetworkProxy, host_matches, is_public};
pub use pty::PtyManager;
#[cfg(windows)]
pub use windows::inner;

/// Configuration handed to the in-namespace helper (as one JSON argument).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InnerSpec {
    pub write_roots: Vec<PathBuf>,
    pub limits: Limits,
    /// Path of the host proxy socket inside the sandbox, when networking is enabled.
    pub proxy_socket: Option<PathBuf>,
    /// Loopback proxy port for platforms without network namespaces.
    #[serde(default)]
    pub proxy_port: Option<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub workspace: PathBuf,
    pub policy: Policy,
    #[serde(default)]
    pub stdin: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub interrupted: bool,
    pub truncated: bool,
}
#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn execute(
        &self,
        request: ExecRequest,
        cancel: CancellationToken,
        stream: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput>;
}
#[derive(Clone)]
pub struct NativeSandbox {
    helper: PathBuf,
}
impl NativeSandbox {
    pub fn new(helper: PathBuf) -> Self {
        Self { helper }
    }
    /// A confined command for long-lived children (PTYs, MCP servers). The returned
    /// guards must outlive the child; they remove per-run temporary state when dropped.
    pub fn confined_command(&self, r: &ExecRequest) -> Result<Launch> {
        ensure!(
            !r.policy.network,
            "interactive and server processes cannot use the network proxy yet"
        );
        ensure!(
            !r.policy.limits.cgroup,
            "cgroup limits for interactive/server processes are not yet supported"
        );
        let mut launch = launch(&self.helper, r, None)?;
        apply_env(&mut launch, false);
        launch.command.kill_on_drop(true);
        Ok(launch)
    }
}

/// A prepared confined process plus resources that must live as long as it does.
pub struct Launch {
    pub command: tokio::process::Command,
    pub guards: Vec<Box<dyn std::any::Any + Send>>,
    env: Vec<(String, String)>,
}

fn apply_env(launch: &mut Launch, batch: bool) {
    let c = &mut launch.command;
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/tmp")
        .env("LANG", "C.UTF-8");
    if batch {
        c.env("TMPDIR", "/tmp").env("PYTHONNOUSERSITE", "1");
    }
    for (key, value) in &launch.env {
        c.env(key, value);
    }
}

#[derive(Debug, Serialize)]
pub struct Diagnostic {
    pub name: String,
    pub available: bool,
    pub detail: String,
}
pub async fn doctor(limits: &Limits) -> Vec<Diagnostic> {
    let _ = limits;
    let mut out = vec![];
    #[cfg(target_os = "linux")]
    {
        out.extend(namespace_diagnostics().await);
        out.push(match cgroup::probe(limits) {
            Ok(controllers) if controllers.is_empty() => Diagnostic {
                name: "cgroup_v2".into(),
                available: false,
                detail: "delegated, but no controllers are enabled for child groups (enable memory, pids and cpu in cgroup.subtree_control; systemd Delegate=yes)".into(),
            },
            Ok(controllers) => Diagnostic {
                name: "cgroup_v2".into(),
                available: true,
                detail: format!("delegated; child controllers: {}", controllers.join(" ")),
            },
            Err(e) => Diagnostic {
                name: "cgroup_v2".into(),
                available: false,
                detail: format!(
                    "{e:#}; rlimits remain available when policy.limits.cgroup=false; requesting cgroup limits fails closed (use delegated service configuration)"
                ),
            },
        });
        let supported = landlock::ABI::V3 as i32;
        out.push(Diagnostic {
            name: "landlock".into(),
            available: std::fs::read_to_string("/sys/kernel/security/lsm")
                .is_ok_and(|s| s.contains("landlock")),
            detail: format!(
                "Landlock ABI {supported} required; effective enforcement checked in each child"
            ),
        });
    }
    #[cfg(target_os = "macos")]
    out.push(Diagnostic {
        name: "seatbelt".into(),
        available: false,
        detail: "Disabled: Seatbelt confinement has not been certified on a native macOS host"
            .into(),
    });
    #[cfg(windows)]
    out.push(Diagnostic {
        name: "windows_isolation".into(),
        available: false,
        detail: "Disabled: unrestricted reads and incomplete network isolation; no production opt-in is accepted".into(),
    });
    for (name, path) in [("python3", "/usr/bin/python3"), ("node", "/usr/bin/node")] {
        out.push(Diagnostic {
            name: name.into(),
            available: Path::new(path).is_file(),
            detail: path.into(),
        });
    }
    out
}

#[cfg(target_os = "linux")]
fn launch(helper: &Path, r: &ExecRequest, proxy: Option<&NetworkProxy>) -> Result<Launch> {
    let plan = plan::plan(
        helper,
        &r.workspace,
        &r.policy,
        &r.argv,
        proxy.map(NetworkProxy::socket),
    )?;
    let mut command = tokio::process::Command::new("/usr/bin/bwrap");
    command.args(plan.args);
    Ok(Launch {
        command,
        guards: vec![],
        env: vec![],
    })
}
#[cfg(any(target_os = "macos", windows))]
fn launch(_helper: &Path, _r: &ExecRequest, _proxy: Option<&NetworkProxy>) -> Result<Launch> {
    bail!(
        "native execution is disabled on this platform until filesystem, network and descendant confinement are certified on a real host"
    )
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn launch(_helper: &Path, _r: &ExecRequest, _proxy: Option<&NetworkProxy>) -> Result<Launch> {
    bail!("native isolation is unavailable on this platform")
}

#[async_trait]
impl Sandbox for NativeSandbox {
    async fn execute(
        &self,
        r: ExecRequest,
        cancel: CancellationToken,
        stream: Option<mpsc::Sender<String>>,
    ) -> Result<ExecOutput> {
        ensure!(!r.argv.is_empty(), "empty command");
        ensure!(!cancel.is_cancelled(), "execution cancelled");
        // The proxy lives exactly as long as this execution.
        let proxy = if !r.policy.network {
            None
        } else if cfg!(target_os = "macos") {
            Some(NetworkProxy::start_tcp(r.policy.allow_domains.clone(), None).await?)
        } else {
            #[cfg(unix)]
            {
                Some(NetworkProxy::start(r.policy.allow_domains.clone(), None).await?)
            }
            #[cfg(not(unix))]
            bail!("tool networking is not available on this platform")
        };
        let mut launched = launch(&self.helper, &r, proxy.as_ref())?;
        apply_env(&mut launched, true);
        let _launch_guards = launched.guards;
        let mut c = launched.command;
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        c.process_group(0);
        #[cfg(target_os = "linux")]
        let _cgroup = if r.policy.limits.cgroup {
            let group = cgroup::Cgroup::create(&r.policy.limits)
                .context("cgroup limits requested but unavailable")?;
            group.attach_before_exec(&mut c)?;
            Some(group)
        } else {
            None
        };
        let mut child = c.spawn().context("unable to start confined process")?;
        let pid = child.id();
        let process_group = ProcessGroup(pid);
        let limit = r.policy.max_output_bytes;
        let stdin = child.stdin.take();
        let input = r.stdin;
        let input_task = tokio::spawn(async move {
            if let Some(mut stdin) = stdin
                && let Some(data) = input
            {
                let _ = stdin.write_all(data.as_bytes()).await;
            }
        });
        let stdout = child.stdout.take().context("missing stdout")?;
        let stderr = child.stderr.take().context("missing stderr")?;
        let exceeded = CancellationToken::new();
        let remaining = Arc::new(AtomicUsize::new(limit));
        let out_task = tokio::spawn(capture(
            stdout,
            remaining.clone(),
            stream.clone(),
            exceeded.clone(),
        ));
        let err_task = tokio::spawn(capture(stderr, remaining, stream, exceeded.clone()));
        let mut interrupted = false;
        let status = tokio::select! {
            res=child.wait()=>res?,
            _=cancel.cancelled()=>{interrupted=true;terminate(&mut child,pid).await?},
            _=exceeded.cancelled()=>{interrupted=true;terminate(&mut child,pid).await?},
            _=tokio::time::sleep(Duration::from_secs(r.policy.timeout_secs))=>{interrupted=true;terminate(&mut child,pid).await?},
        };
        // Kill descendants even if the leader exited normally while a child still
        // holds stdin/stdout. Aborting this future also triggers the same cleanup.
        drop(process_group);
        input_task.abort();
        let _ = input_task.await;
        let (stdout, truncated_out) = out_task.await??;
        let (stderr, truncated_err) = err_task.await??;
        if status.code() == Some(1)
            && (stderr.starts_with("bwrap:") || stderr.contains("sandbox enforcement failed"))
        {
            bail!("sandbox unavailable: {}", stderr.trim());
        }
        Ok(ExecOutput {
            code: status.code(),
            stdout,
            stderr,
            interrupted,
            truncated: truncated_out || truncated_err,
        })
    }
}
async fn capture<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    remaining: Arc<AtomicUsize>,
    stream: Option<mpsc::Sender<String>>,
    exceeded: CancellationToken,
) -> Result<(String, bool)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let previous = remaining
            .try_update(Ordering::AcqRel, Ordering::Acquire, |left| {
                Some(left.saturating_sub(n))
            })
            .unwrap_or(0);
        let keep = n.min(previous);
        if keep > 0
            && let Some(s) = &stream
        {
            let _ = s.try_send(String::from_utf8_lossy(&buf[..keep]).into_owned());
        }
        data.extend_from_slice(&buf[..keep]);
        if keep < n {
            truncated = true;
            exceeded.cancel();
        }
    }
    Ok((String::from_utf8_lossy(&data).into_owned(), truncated))
}
/// RAII cleanup also runs when a caller drops/cancels the execution future.
#[cfg_attr(not(unix), allow(dead_code))]
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: process group belongs to the child started by this execution.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

async fn terminate(
    child: &mut tokio::process::Child,
    #[cfg_attr(not(unix), allow(unused_variables))] pid: Option<u32>,
) -> Result<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGINT);
        }
    }
    if let Ok(status) = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await {
        return Ok(status?);
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if let Ok(status) = tokio::time::timeout(Duration::from_millis(1500), child.wait()).await {
        return Ok(status?);
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    child.start_kill()?;
    Ok(child.wait().await?)
}

/// Probes namespace creation step by step so a failure names the missing capability.
#[cfg(target_os = "linux")]
async fn namespace_diagnostics() -> Vec<Diagnostic> {
    let steps: [(&str, &[&str]); 4] = [
        ("user_namespace", &["--unshare-user"]),
        ("pid_namespace", &["--unshare-user", "--unshare-pid"]),
        ("net_namespace", &["--unshare-user", "--unshare-net"]),
        ("all_namespaces", &["--unshare-all"]),
    ];
    let mut out = Vec::new();
    for (name, flags) in steps {
        let result = tokio::process::Command::new("/usr/bin/bwrap")
            .args(flags)
            .args([
                "--ro-bind",
                "/",
                "/",
                "--die-with-parent",
                "--",
                "/bin/true",
            ])
            .env_clear()
            .output()
            .await;
        out.push(match result {
            Ok(x) if x.status.success() => Diagnostic {
                name: name.into(),
                available: true,
                detail: "verified".into(),
            },
            Ok(x) => Diagnostic {
                name: name.into(),
                available: false,
                detail: format!(
                    "{} {}",
                    String::from_utf8_lossy(&x.stderr).trim(),
                    apparmor_hint()
                ),
            },
            Err(e) => Diagnostic {
                name: name.into(),
                available: false,
                detail: e.to_string(),
            },
        });
    }
    out
}

#[cfg(target_os = "linux")]
fn apparmor_hint() -> String {
    let restricted =
        std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
            .is_ok_and(|v| v.trim() == "1");
    if restricted {
        "Host AppArmor restricts unprivileged user namespaces for unprofiled programs; install deploy/apparmor/layeragent-bwrap (see docs) rather than disabling the protection.".into()
    } else {
        "Check kernel.unprivileged_userns_clone and user.max_user_namespaces.".into()
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn output_limit_also_caps_streaming_and_cancels_the_workload() {
        let (reader, mut writer) = tokio::io::duplex(32);
        let (tx, mut rx) = mpsc::channel(8);
        let exceeded = CancellationToken::new();
        tokio::spawn(async move {
            writer.write_all(b"0123456789").await.unwrap();
        });
        let (text, truncated) = capture(
            reader,
            Arc::new(AtomicUsize::new(5)),
            Some(tx),
            exceeded.clone(),
        )
        .await
        .unwrap();
        assert_eq!(text, "01234");
        assert!(truncated && exceeded.is_cancelled());
        assert_eq!(rx.recv().await.as_deref(), Some("01234"));
        assert!(rx.recv().await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forced_termination_kills_descendants_that_ignore_signals() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "trap '' INT TERM; sleep 60 & wait"])
            .process_group(0)
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let pid = child.id();
        let cleanup = ProcessGroup(pid);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let status = tokio::time::timeout(Duration::from_secs(5), terminate(&mut child, pid))
            .await
            .unwrap()
            .unwrap();
        assert!(!status.success());
        drop(cleanup);
        if let Some(pid) = pid {
            // The group can briefly contain a zombie awaiting the host's reaper;
            // no live child may remain after escalation.
            let table = std::fs::read_dir("/proc").unwrap();
            for entry in table.flatten() {
                let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some((_, fields)) = stat.rsplit_once(") ") else {
                    continue;
                };
                let fields: Vec<_> = fields.split_whitespace().collect();
                if fields.get(2).is_some_and(|p| *p == pid.to_string()) {
                    assert_eq!(fields[0], "Z", "live descendant: {stat}");
                }
            }
        }
    }
}
