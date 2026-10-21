//! Outer browser isolation. Linux exposes only runtime files and an ephemeral
//! profile; networking and DevTools cross the namespace through dedicated UNIX
//! sockets. Chromium's own sandbox remains mandatory inside this boundary.
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use anyhow::bail;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserInnerSpec {
    pub profile: PathBuf,
    pub proxy_socket: PathBuf,
}

pub fn browser_command(
    helper: &Path,
    executable: &Path,
    profile: &Path,
    proxy_socket: &Path,
    args: &[String],
) -> Result<tokio::process::Command> {
    #[cfg(target_os = "linux")]
    {
        use anyhow::{Context, ensure};
        let executable = executable
            .canonicalize()
            .context("browser executable missing")?;
        let helper = helper
            .canonicalize()
            .context("browser confinement helper missing")?;
        ensure!(
            helper.is_file() && executable.is_file(),
            "browser and helper must be files"
        );
        let browser_dir = executable
            .parent()
            .context("browser has no parent directory")?;
        let filename = executable.file_name().context("browser has no file name")?;
        let profile = profile.canonicalize()?;
        let mut command = tokio::process::Command::new("/usr/bin/bwrap");
        command.args([
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--cap-drop",
            "ALL",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--dir",
            "/run",
            "--dir",
            "/home",
        ]);
        for path in [
            "/usr",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/etc/ld.so.cache",
            "/etc/alternatives",
            "/etc/fonts",
            "/etc/ssl",
            "/etc/passwd",
            "/etc/group",
        ] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
        command.args(["--dir", "/layeragent-browser"]);
        command
            .arg("--ro-bind")
            .arg(&executable)
            .arg(Path::new("/layeragent-browser").join(filename));
        // A custom browser path must never expose its entire parent directory,
        // which could be the user's home or a repository containing credentials.
        for name in [
            "chrome_100_percent.pak",
            "chrome_200_percent.pak",
            "icudtl.dat",
            "locales",
            "resources.pak",
            "v8_context_snapshot.bin",
            "libEGL.so",
            "libGLESv2.so",
            "libvulkan.so.1",
            "libvk_swiftshader.so",
            "vk_swiftshader_icd.json",
            "chrome_crashpad_handler",
        ] {
            let resource = browser_dir.join(name);
            if resource.exists() {
                let resolved = resource.canonicalize()?;
                ensure!(
                    resolved.starts_with(browser_dir),
                    "browser resource symlink leaves its install directory"
                );
                command
                    .arg("--ro-bind")
                    .arg(resolved)
                    .arg(Path::new("/layeragent-browser").join(name));
            }
        }
        command
            .arg("--ro-bind")
            .arg(&helper)
            .arg("/layeragent-helper")
            .arg("--bind")
            .arg(&profile)
            .arg("/profile")
            .arg("--bind")
            .arg(proxy_socket)
            .arg("/run/layeragent-proxy.sock")
            .args([
                "--chdir",
                "/profile",
                "--",
                "/layeragent-helper",
                "__browser_inner",
            ])
            .arg(serde_json::to_string(&BrowserInnerSpec {
                profile: "/profile".into(),
                proxy_socket: "/run/layeragent-proxy.sock".into(),
            })?)
            .arg("--")
            .arg(Path::new("/layeragent-browser").join(filename))
            .args(args);
        Ok(command)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (helper, executable, profile, proxy_socket, args);
        bail!("managed browser confinement requires a supporting Linux host")
    }
}

pub fn browser_inner(spec: &BrowserInnerSpec, argv: &[String]) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use anyhow::{Context, ensure};
        use std::{
            net::TcpStream,
            os::unix::{
                net::{UnixListener, UnixStream},
                process::ExitStatusExt,
            },
        };
        ensure!(!argv.is_empty(), "empty browser command");
        super::inner::apply_rlimits(&super::Limits {
            max_pids: Some(512),
            cpu_secs: Some(120),
            file_size_bytes: Some(256 << 20),
            open_files: Some(1024),
            ..Default::default()
        })?;
        let proxy = std::net::TcpListener::bind(("127.0.0.1", 3128))?;
        let socket = spec.proxy_socket.clone();
        std::thread::spawn(move || {
            for client in proxy.incoming().flatten() {
                if let Ok(upstream) = UnixStream::connect(&socket) {
                    relay(client, upstream);
                }
            }
        });
        let control = UnixListener::bind(spec.profile.join("devtools.sock"))?;
        let profile = spec.profile.clone();
        std::thread::spawn(move || {
            for client in control.incoming().flatten() {
                let port = devtools_port(&profile).ok();
                if let Some(port) = port
                    && let Ok(upstream) = TcpStream::connect(("127.0.0.1", port))
                {
                    relay(client, upstream);
                }
            }
        });
        let mut child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .env("HOME", &spec.profile)
            .env("TMPDIR", "/tmp")
            .spawn()
            .context("starting Chromium inside browser namespaces")?;
        let status = child.wait()?;
        std::process::exit(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        );
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (spec, argv);
        bail!("browser confinement is unsupported on this platform")
    }
}

/// The browser may replace this entry at any time. Never follow a link, open a
/// blocking FIFO, or allocate an unbounded string inside the helper process.
#[cfg(target_os = "linux")]
fn devtools_port(profile: &Path) -> Result<u16> {
    use anyhow::{Context, ensure};
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    const MAX_METADATA: u64 = 4096;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(profile.join("DevToolsActivePort"))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_METADATA,
        "invalid browser control metadata file"
    );
    let mut text = String::new();
    file.take(MAX_METADATA + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_METADATA,
        "browser control metadata exceeds its limit"
    );
    let port = text
        .lines()
        .next()
        .context("missing browser control port")?
        .parse::<u16>()?;
    ensure!(port != 0, "invalid browser control port");
    Ok(port)
}

#[cfg(target_os = "linux")]
pub(crate) fn relay<A, B>(left: A, right: B)
where
    A: std::io::Read + std::io::Write + Send + CloneStream + 'static,
    B: std::io::Read + std::io::Write + Send + CloneStream + 'static,
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
    if CONNECTIONS.fetch_add(1, Ordering::AcqRel) >= 64 {
        CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
        return;
    }
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
        }
    }
    std::thread::spawn(move || {
        let _permit = Permit;
        let (Ok(mut left_in), Ok(mut right_out)) = (left.clone_stream(), right.clone_stream())
        else {
            return;
        };
        let mut left_out = left;
        let mut right_in = right;
        let forward = std::thread::spawn(move || {
            let _ = std::io::copy(&mut left_in, &mut right_out);
            right_out.close_write();
        });
        let _ = std::io::copy(&mut right_in, &mut left_out);
        left_out.close_write();
        let _ = forward.join();
    });
}

#[cfg(target_os = "linux")]
pub(crate) trait CloneStream: Sized {
    fn clone_stream(&self) -> std::io::Result<Self>;
    fn close_write(&self);
}
#[cfg(target_os = "linux")]
macro_rules! stream_impl {
    ($ty:ty) => {
        impl CloneStream for $ty {
            fn clone_stream(&self) -> std::io::Result<Self> {
                self.try_clone()
            }
            fn close_write(&self) {
                let _ = self.shutdown(std::net::Shutdown::Write);
            }
        }
    };
}
#[cfg(target_os = "linux")]
stream_impl!(std::net::TcpStream);
#[cfg(target_os = "linux")]
stream_impl!(std::os::unix::net::UnixStream);

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn browser_plan_never_shares_network_or_exposes_home_or_workspace() {
        let profile = tempfile::tempdir().unwrap();
        let helper = std::env::current_exe().unwrap();
        let command = browser_command(
            &helper,
            Path::new("/usr/bin/true"),
            profile.path(),
            Path::new("/tmp/proxy.sock"),
            &["about:blank".into()],
        )
        .unwrap();
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--unshare-all".into()));
        assert!(!args.contains(&"--share-net".into()));
        assert!(!args.contains(&"--no-sandbox".into()));
        assert!(args.contains(&"__browser_inner".into()));
        if let Some(home) = std::env::var_os("HOME") {
            assert!(!args.iter().any(|a| a == &home.to_string_lossy()));
        }
        assert!(
            !args.windows(3).any(|a| a[0] == "--ro-bind"
                && a[1] == "/usr/bin"
                && a[2] == "/layeragent-browser")
        );
    }
    #[test]
    fn helper_control_metadata_is_bounded_and_rejects_links_fifos_and_devices() {
        use std::{
            ffi::CString,
            os::unix::{ffi::OsStrExt, fs::symlink},
        };
        let profile = tempfile::tempdir().unwrap();
        let path = profile.path().join("DevToolsActivePort");
        std::fs::write(&path, "12345\n/devtools/browser/control\n").unwrap();
        assert_eq!(devtools_port(profile.path()).unwrap(), 12345);
        std::fs::write(&path, "0\n").unwrap();
        assert!(devtools_port(profile.path()).is_err());
        std::fs::write(&path, "1".repeat(4097)).unwrap();
        assert!(devtools_port(profile.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        symlink("/dev/zero", &path).unwrap();
        assert!(devtools_port(profile.path()).is_err());
        std::fs::remove_file(&path).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path; this creates only a private test FIFO.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(devtools_port(profile.path()).is_err());
    }
}
