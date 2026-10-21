//! The agent's own browser.
//!
//! By default a dedicated Chromium is launched with a throw-away profile and all of its
//! traffic forced through the confined [`NetworkProxy`], so reachability is decided by
//! `browser.allow_domains` and private/loopback addresses are never reachable. The
//! browser uses separate Linux mount/PID/network namespaces. Visible windows and host
//! browser attachment remain disabled until their host access can be isolated. Sessions belong to a turn (the "owner") and are torn down with it.
mod cdp;
pub mod install;
mod page;

use anyhow::{Context, Result, bail, ensure};
use layeragent_config::BrowserConfig;
use layeragent_sandbox::{NetworkDecision, NetworkProxy};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

pub use page::{Page, check_url};

/// A browser process we started, and everything that must die with it.
struct Launched {
    child: Mutex<tokio::process::Child>,
    profile: PathBuf,
    proxy: NetworkProxy,
}

impl Launched {
    fn shutdown(&self) {
        if let Ok(mut child) = self.child.lock() {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                // SAFETY: this is the process group created for our child.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
        }
    }
}
impl Drop for Launched {
    fn drop(&mut self) {
        self.shutdown();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

struct ProfileGuard(Option<PathBuf>);
impl Drop for ProfileGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

pub struct Session {
    // Dropped first: closes the tab and the DevTools socket before the process is killed.
    pub page: Page,
    launched: Option<Launched>,
    pub attached: bool,
}

impl Session {
    /// Destinations the proxy allowed or refused for this browser so far.
    pub fn proxy_decisions(&self) -> Vec<NetworkDecision> {
        self.launched
            .as_ref()
            .map(|l| l.proxy.decisions())
            .unwrap_or_default()
    }
}

#[derive(Default)]
pub struct BrowserManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    launch_gate: tokio::sync::Mutex<()>,
    /// Tests only: lets the browser reach loopback fixtures.
    allow_private: bool,
    helper: Option<PathBuf>,
}

fn has_display() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
        || std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

impl BrowserManager {
    #[doc(hidden)]
    pub fn for_tests_allowing_private_network() -> Self {
        let mut manager = Self::default();
        manager.allow_private = true;
        manager.helper = std::env::var_os("LAYERAGENT_BROWSER_HELPER").map(PathBuf::from);
        manager
    }

    pub fn get(&self, owner: &str) -> Result<Arc<Session>> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("browser registry poisoned"))?
            .get(owner)
            .cloned()
            .context("no browser is open; call browser_open first")
    }

    fn lookup(&self, owner: &str) -> Option<Arc<Session>> {
        self.sessions.lock().ok()?.get(owner).cloned()
    }

    /// A confined production manager with an explicit trusted helper binary.
    /// Useful for embedding the library in a process other than the LayerAgent CLI.
    pub fn with_helper(helper: PathBuf) -> Self {
        let mut manager = Self::default();
        manager.helper = Some(helper);
        manager
    }

    /// Returns this owner's browser, launching the managed one on first use.
    pub async fn open(
        &self,
        owner: &str,
        home: &Path,
        cfg: &BrowserConfig,
        visible: Option<bool>,
    ) -> Result<Arc<Session>> {
        ensure!(
            cfg.enabled,
            "the browser is disabled (browser.enabled = false)"
        );
        if let Some(existing) = self.lookup(owner) {
            return Ok(existing);
        }
        let _gate = self.launch_gate.lock().await;
        if let Some(existing) = self.lookup(owner) {
            return Ok(existing);
        }
        ensure!(
            !cfg.no_sandbox,
            "browser.no_sandbox is forbidden; install the scoped AppArmor profile instead"
        );
        ensure!(
            cfg!(target_os = "linux"),
            "managed browser isolation is implemented only for Linux; host qualification is required"
        );
        let visible = visible.unwrap_or(cfg.visible);
        ensure!(
            !visible,
            "visible browser mode is disabled until display access can be isolated"
        );
        ensure!(
            !visible || has_display(),
            "a visible browser needs a display (DISPLAY or WAYLAND_DISPLAY is not set)"
        );
        let executable =
            install::ensure_browser(home, cfg.executable.as_deref(), cfg.auto_install).await?;
        let helper = match &self.helper {
            Some(path) => path.clone(),
            None => std::env::current_exe()?,
        };
        let (launched, ws) = launch(&helper, &executable, cfg, visible, self.allow_private).await?;
        #[cfg(unix)]
        let cdp =
            Arc::new(cdp::Cdp::connect_unix(&ws, &launched.profile.join("devtools.sock")).await?);
        #[cfg(not(unix))]
        let cdp = Arc::new(cdp::Cdp::connect(&ws).await?);
        let page = Page::open(cdp, Duration::from_secs(cfg.page_timeout_secs)).await?;
        let session = Arc::new(Session {
            page,
            launched: Some(launched),
            attached: false,
        });
        self.insert(owner, session.clone())?;
        Ok(session)
    }

    /// Attaches to a browser the user started with `--remote-debugging-port`.
    pub async fn attach(&self, owner: &str, cfg: &BrowserConfig) -> Result<Arc<Session>> {
        let _ = (owner, cfg);
        bail!(
            "attaching to a host browser is disabled: its filesystem, network and existing account access are outside LayerAgent isolation"
        )
    }

    fn insert(&self, owner: &str, session: Arc<Session>) -> Result<()> {
        self.sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("browser registry poisoned"))?
            .insert(owner.to_string(), session);
        Ok(())
    }

    /// Ends a session: closes its tab and, if we started the browser, kills it.
    pub fn close_owner(&self, owner: &str) {
        let removed = self.sessions.lock().ok().and_then(|mut s| s.remove(owner));
        if let Some(session) = removed {
            if let Some(launched) = &session.launched {
                launched.shutdown();
            }
            // Close the tab of an attached browser; a launched one is killed on drop.
            if session.attached {
                tokio::spawn(async move { session.page.close().await });
            }
        }
    }

    pub fn open_count(&self) -> usize {
        self.sessions.lock().map_or(0, |s| s.len())
    }
}

impl Drop for BrowserManager {
    fn drop(&mut self) {
        if let Ok(sessions) = self.sessions.get_mut() {
            for session in sessions.values() {
                if let Some(launched) = &session.launched {
                    launched.shutdown();
                }
            }
        }
    }
}

/// Which browser would be used, for `layeragent browser status`.
pub fn status(home: &Path, cfg: &BrowserConfig) -> serde_json::Value {
    let found = install::locate(home, cfg.executable.as_deref());
    serde_json::json!({
        "enabled": cfg.enabled,
        "execution_supported": cfg!(target_os = "linux"),
        "isolation": "outer Linux namespaces plus mandatory Chromium sandbox",
        "unsafe_modes_supported": false,
        "browser": found.as_ref().ok().and_then(|p| p.as_ref()).map(|p| p.display().to_string()),
        "error": found.as_ref().err().map(|e| e.to_string()),
        "will_download_on_first_use": cfg.auto_install && matches!(found, Ok(None)),
        "managed": install::find_managed(home).map(|p| p.display().to_string()),
        "allow_domains": cfg.allow_domains,
        "visible_by_default": cfg.visible,
    })
}

// Files written by Chromium are untrusted even though their parent is private.
// Never follow a symlink back into the host or block on a FIFO/device.
fn read_profile_file(path: &Path, max_bytes: u64) -> Result<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "browser profile entry must be a regular file"
    );
    let mut text = String::new();
    file.take(max_bytes).read_to_string(&mut text)?;
    Ok(text)
}

async fn launch(
    helper: &Path,
    executable: &Path,
    cfg: &BrowserConfig,
    visible: bool,
    allow_private: bool,
) -> Result<(Launched, String)> {
    let profile = std::env::temp_dir().join(format!(
        "layeragent-browser-{}",
        layeragent_protocol::new_id()
    ));
    std::fs::create_dir(&profile)?;
    let mut profile_guard = ProfileGuard(Some(profile.clone()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(unix)]
    let proxy = NetworkProxy::start_with(cfg.allow_domains.clone(), None, allow_private).await?;
    #[cfg(not(unix))]
    let proxy =
        NetworkProxy::start_tcp_with(cfg.allow_domains.clone(), None, allow_private).await?;
    let mut browser_args = tokio::process::Command::new(executable);
    browser_args
        .arg("--user-data-dir=/profile")
        .arg("--remote-debugging-port=0")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--proxy-server=http://127.0.0.1:3128")
        // `<-loopback>` removes the implicit loopback bypass: even localhost goes to the proxy,
        // which refuses it, so the agent's browser cannot reach local services.
        .arg("--proxy-bypass-list=<-loopback>")
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-quic",
            // Resolver mappings apply to the proxy address too. Keep only
            // the confined proxy resolvable; site DNS stays host-side.
            "--host-resolver-rules=MAP * ~NOTFOUND, EXCLUDE 127.0.0.1",
            "--disable-sync",
            "--disable-extensions",
            "--disable-component-update",
            "--disable-default-apps",
            "--disable-translate",
            "--disable-dev-shm-usage",
            "--mute-audio",
            "--password-store=basic",
            "--use-mock-keychain",
            "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
            "--window-size=1280,900",
        ]);
    if !visible {
        browser_args.args(["--headless=new", "--disable-gpu", "--hide-scrollbars"]);
    }
    ensure!(!cfg.no_sandbox, "Chromium sandbox cannot be disabled");
    browser_args.arg("about:blank");
    let args = browser_args
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut command =
        layeragent_sandbox::browser_command(helper, executable, &profile, proxy.socket(), &args)?;
    command
        .env_clear()
        .env("HOME", "/profile")
        .env("LANG", "C.UTF-8")
        .env("PATH", "/usr/local/bin:/usr/bin:/bin");
    for var in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
    ] {
        if visible && let Some(value) = std::env::var_os(var) {
            command.env(var, value);
        }
    }
    let log = std::fs::File::create(profile.join("chrome.log"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .with_context(|| format!("starting {}", executable.display()))?;
    profile_guard.0 = None;
    let mut launched = Launched {
        child: Mutex::new(child),
        profile: profile.clone(),
        proxy,
    };
    let active_port = profile.join("DevToolsActivePort");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(text) = read_profile_file(&active_port, 4096) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                ensure!(
                    port.chars().all(|c| c.is_ascii_digit()),
                    "unexpected DevTools port file"
                );
                ensure!(path.starts_with("/devtools/"), "unexpected DevTools path");
                return Ok((launched, format!("ws://127.0.0.1:{port}{path}")));
            }
        }
        if let Some(status) = launched
            .child
            .get_mut()
            .map_err(|_| anyhow::anyhow!("browser process lock poisoned"))?
            .try_wait()?
        {
            let log = read_profile_file(&profile.join("chrome.log"), 16 * 1024).unwrap_or_default();
            // The first fatal line explains the exit; the stack trace after it is noise.
            let tail = log
                .lines()
                .find(|l| l.contains("FATAL") || l.contains("ERROR:"))
                .map(str::to_owned)
                .unwrap_or_else(|| log.chars().take(400).collect());
            let hint = if !cfg.no_sandbox {
                " (Chromium's own sandbox probably could not start: on Ubuntu 24.04+ install deploy/apparmor/install.sh, which also covers the managed browser, and keep browser.no_sandbox disabled)"
            } else {
                ""
            };
            bail!(
                "the browser exited immediately ({status}){hint}: {}",
                tail.trim()
            );
        }
        if tokio::time::Instant::now() > deadline {
            bail!("the browser did not expose its DevTools port in time");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod isolation_tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn explicit_shutdown_terminates_the_browser_even_while_a_session_is_referenced() {
        let profile = tempfile::tempdir().unwrap();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("60").process_group(0).kill_on_drop(true);
        let child = command.spawn().unwrap();
        let proxy = NetworkProxy::start_tcp(vec![], None).await.unwrap();
        let mut launched = Launched {
            child: Mutex::new(child),
            profile: profile.path().into(),
            proxy,
        };
        launched.shutdown();
        let status = tokio::time::timeout(
            Duration::from_secs(2),
            launched.child.get_mut().unwrap().wait(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!status.success());
    }

    #[cfg(unix)]
    #[test]
    fn profile_metadata_never_follows_host_symlinks_and_reads_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let secret = root.path().join("secret");
        std::fs::write(&secret, "sensitive host content").unwrap();
        let link = root.path().join("DevToolsActivePort");
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        assert!(read_profile_file(&link, 100).is_err());
        assert_eq!(read_profile_file(&secret, 9).unwrap(), "sensitive");
        assert!(read_profile_file(root.path(), 100).is_err());
    }

    #[tokio::test]
    async fn unsafe_modes_are_rejected_before_download_or_launch() {
        let home = tempfile::tempdir().unwrap();
        let manager = BrowserManager::default();
        let cfg = BrowserConfig {
            no_sandbox: true,
            ..Default::default()
        };
        assert!(
            manager
                .open("owner", home.path(), &cfg, None)
                .await
                .is_err()
        );
        assert!(
            manager
                .open("owner", home.path(), &BrowserConfig::default(), Some(true))
                .await
                .is_err()
        );
        assert!(
            manager
                .attach("owner", &BrowserConfig::default())
                .await
                .is_err()
        );
        assert_eq!(manager.open_count(), 0);
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }
}
