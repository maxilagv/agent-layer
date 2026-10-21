#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used)]
use anyhow::Result;
use layeragent_sandbox::{ExecRequest, NativeSandbox, Sandbox};
use tokio_util::sync::CancellationToken;

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires working Bubblewrap namespaces and Landlock ABI 3; run explicitly on a supported host"]
async fn kernel_confinement_denies_secrets_metadata_and_host_writes() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join(".env"), "SHOULD_NOT_BE_VISIBLE")?;
    std::fs::create_dir(root.path().join(".git"))?;
    std::fs::write(root.path().join(".git/config"), "original")?;
    let code = r#"import pathlib, json
checks = {}
try:
    checks['secret_hidden'] = pathlib.Path('.env').read_text() == ''
except (PermissionError, FileNotFoundError):
    checks['secret_hidden'] = True
for key, path in [('metadata_readonly', '.git/config'), ('host_readonly', '/usr/layeragent-forbidden')]:
    try:
        pathlib.Path(path).write_text('forbidden')
        checks[key] = False
    except OSError:
        checks[key] = True
pathlib.Path('allowed.txt').write_text('workspace edit')
print(json.dumps(checks))
"#;
    let sandbox = NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into());
    let output = sandbox
        .execute(
            ExecRequest {
                argv: vec!["python3".into(), "-I".into(), "-c".into(), code.into()],
                workspace: root.path().into(),
                policy: Default::default(),
                stdin: None,
            },
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(output.code, Some(0), "{}", output.stderr);
    let checks: serde_json::Value = serde_json::from_str(&output.stdout)?;
    assert!(
        checks
            .as_object()
            .is_some_and(|m| m.values().all(|v| v == true))
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join(".env"))?,
        "SHOULD_NOT_BE_VISIBLE"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join(".git/config"))?,
        "original"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("allowed.txt"))?,
        "workspace edit"
    );
    Ok(())
}

/// Runs the helper stage directly. Landlock, seccomp and rlimits need no namespaces,
/// so this exercises real kernel enforcement even where Bubblewrap is unavailable.
fn helper(spec: serde_json::Value, script: &str) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_layeragent"))
        .args([
            "__sandbox_inner",
            &spec.to_string(),
            "--",
            "python3",
            "-I",
            "-c",
            script,
        ])
        .output()
        .expect("helper starts")
}

fn spec(write_root: &std::path::Path, proxy: Option<&std::path::Path>) -> serde_json::Value {
    serde_json::json!({
        "write_roots": [write_root],
        "limits": {"file_size_bytes": 1000, "max_pids": null, "memory_bytes": null,
                   "cpu_secs": null, "open_files": null, "cgroup": false, "cpu_percent": null},
        "proxy_socket": proxy,
    })
}

#[test]
fn helper_stage_enforces_landlock_rlimits_and_socket_filter() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir_in(std::env::var("HOME").unwrap_or("/var/tmp".into())).unwrap();
    let script = format!(
        r#"import json, socket
r = {{}}
def attempt(name, fn):
    try:
        fn(); r[name] = True
    except OSError:
        r[name] = False
attempt('write_root', lambda: open('{root}/a.txt', 'w').write('x'))
attempt('write_outside', lambda: open('{outside}/b.txt', 'w').write('x'))
attempt('fsize', lambda: open('{root}/big.txt', 'w').write('x' * 100000))
attempt('unix_socket', lambda: socket.socket(socket.AF_UNIX))
attempt('raw_socket', lambda: socket.socket(socket.AF_INET, socket.SOCK_RAW))
print(json.dumps(r))"#,
        root = root.path().display(),
        outside = outside.path().display()
    );
    let out = helper(spec(root.path(), None), &script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(r["write_root"], true);
    assert_eq!(
        r["write_outside"], false,
        "landlock must deny writes outside roots"
    );
    assert_eq!(r["fsize"], false, "RLIMIT_FSIZE must apply");
    assert_eq!(r["unix_socket"], false);
    assert_eq!(r["raw_socket"], false);
    assert!(!outside.path().join("b.txt").exists());
}

#[tokio::test]
async fn helper_bridges_loopback_to_the_confined_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = upstream.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = upstream.accept().await {
            tokio::spawn(async move {
                let mut b = [0u8; 1024];
                let n = s.read(&mut b).await.unwrap_or(0);
                let body = String::from_utf8_lossy(&b[..n]).into_owned();
                let reply = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK"
                    .to_string();
                let _ = s.write_all(reply.as_bytes()).await;
                assert!(body.starts_with("GET /hello HTTP/1.1"), "{body}");
            });
        }
    });
    let proxy = layeragent_sandbox::NetworkProxy::start_with(vec!["127.0.0.1".into()], None, true)
        .await
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let sock = proxy.socket().to_path_buf();
    let spec = spec(root.path(), Some(&sock));
    let script = format!(
        "import urllib.request; print(urllib.request.urlopen('http://127.0.0.1:{port}/hello', timeout=5).read().decode())"
    );
    let out = tokio::task::spawn_blocking(move || helper(spec, &script))
        .await
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "OK",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        proxy
            .decisions()
            .iter()
            .any(|d| d.allowed && d.port == port)
    );
}

fn request(argv: &[&str], policy: layeragent_config::Policy, dir: &std::path::Path) -> ExecRequest {
    ExecRequest {
        argv: argv.iter().map(|s| s.to_string()).collect(),
        workspace: dir.into(),
        policy,
        stdin: None,
    }
}

#[tokio::test]
#[ignore = "requires working Bubblewrap namespaces and Internet HTTPS access to example.org for the positive control; see deploy/apparmor"]
async fn sandbox_network_is_closed_unless_the_proxy_allows_the_destination() -> Result<()> {
    let root = tempfile::tempdir()?;
    let sandbox = NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into());
    let probe = "import socket,sys\ntry:\n socket.create_connection(('1.1.1.1',53),timeout=3); print('REACHED')\nexcept OSError: print('closed')";
    // No network policy: no route at all.
    let out = sandbox
        .execute(
            request(
                &["python3", "-I", "-c", probe],
                Default::default(),
                root.path(),
            ),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(out.stdout.trim(), "closed", "{}", out.stderr);
    // Network enabled: direct sockets stay closed, only the proxy path exists.
    let policy = layeragent_config::Policy {
        network: true,
        allow_domains: vec!["example.org".into()],
        ..Default::default()
    };
    let out = sandbox
        .execute(
            request(&["python3", "-I", "-c", probe], policy.clone(), root.path()),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(out.stdout.trim(), "closed", "{}", out.stderr);
    // Same origin, request bytes and UA in both cases: only allow_domains differs.
    // DNS/TLS/timeouts and arbitrary exceptions are failures, never policy evidence.
    let via_proxy = r#"import urllib.request,urllib.error,http.client,json
request=urllib.request.Request('https://example.org/',headers={'User-Agent':'LayerAgentNativeVerification/0.1'})
try:
    with urllib.request.urlopen(request,timeout=15) as r:
        body=r.read(65536).decode('utf-8')
        result={'status':r.status,'expected_content':'Example Domain' in body}
except urllib.error.URLError as error:
    if not isinstance(error.reason,http.client.RemoteDisconnected):
        raise
    result={'proxy_rejected':True,'rejection_class':type(error.reason).__name__}
except http.client.RemoteDisconnected as error:
    result={'proxy_rejected':True,'rejection_class':type(error).__name__}
print(json.dumps(result))
"#;
    let mut deny_policy = policy.clone();
    deny_policy.allow_domains = vec!["example.net".into()];
    let out = sandbox
        .execute(
            request(
                &["python3", "-I", "-c", via_proxy],
                deny_policy,
                root.path(),
            ),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let denial: serde_json::Value = serde_json::from_str(&out.stdout)?;
    assert_eq!(
        denial["proxy_rejected"], true,
        "unlisted public destination was reachable"
    );
    assert_eq!(denial["rejection_class"], "RemoteDisconnected");
    // The identical request must succeed when the origin is explicitly allowed.
    let out = sandbox
        .execute(
            request(&["python3", "-I", "-c", via_proxy], policy, root.path()),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    let control: serde_json::Value = serde_json::from_str(&out.stdout)?;
    assert_eq!(control["status"], 200);
    assert_eq!(control["expected_content"], true);

    Ok(())
}

#[tokio::test]
#[ignore = "requires working Bubblewrap namespaces; see deploy/apparmor"]
async fn sandbox_descendants_limits_and_cancellation_are_enforced() -> Result<()> {
    let root = tempfile::tempdir()?;
    let sandbox = NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into());
    // A descendant outliving its parent must die with cancellation.
    let token = CancellationToken::new();
    let child = token.clone();
    let task = tokio::spawn({
        let dir = root.path().to_path_buf();
        async move {
            NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into())
                .execute(
                    request(
                        &["sh", "-c", "(sleep 300 &) ; sleep 300"],
                        Default::default(),
                        &dir,
                    ),
                    child,
                    None,
                )
                .await
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    token.cancel();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), task).await???;
    assert!(out.interrupted);
    // Memory limit stops an allocation bomb.
    let policy = layeragent_config::Policy {
        limits: layeragent_config::Limits {
            memory_bytes: Some(256 << 20),
            ..Default::default()
        },
        ..Default::default()
    };
    let out = sandbox
        .execute(
            request(
                &[
                    "python3",
                    "-I",
                    "-c",
                    "x = bytearray(1 << 30)\nprint('ALLOCATED')",
                ],
                policy,
                root.path(),
            ),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert!(!out.stdout.contains("ALLOCATED"), "{}", out.stderr);
    Ok(())
}

#[tokio::test]
#[ignore = "requires working Bubblewrap namespaces; see deploy/apparmor"]
async fn sandbox_startup_latency_is_measured() -> Result<()> {
    let root = tempfile::tempdir()?;
    let sandbox = NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into());
    let mut samples = Vec::new();
    for _ in 0..15 {
        let start = std::time::Instant::now();
        let out = sandbox
            .execute(
                request(&["/bin/true"], Default::default(), root.path()),
                CancellationToken::new(),
                None,
            )
            .await?;
        assert_eq!(out.code, Some(0), "{}", out.stderr);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.total_cmp(b));
    let median = samples[samples.len() / 2];
    eprintln!(
        "sandbox startup median {median:.1} ms, max {:.1} ms",
        samples[samples.len() - 1]
    );
    assert!(median < 500.0, "median startup {median} ms");
    Ok(())
}

#[test]
fn helper_allows_normal_build_work_but_denies_namespace_escape_and_fd_exhaustion() {
    let root = tempfile::tempdir().unwrap();
    let mut config = spec(root.path(), None);
    config["limits"]["open_files"] = serde_json::json!(64);
    let script = r#"import os, json, threading, subprocess, ctypes, errno
r = {}
t = threading.Thread(target=lambda: r.update(thread=True))
t.start(); t.join()
r['subprocess'] = subprocess.check_output(['/bin/echo', 'legitimate']).decode().strip() == 'legitimate'
libc = ctypes.CDLL(None, use_errno=True)
r['namespace_denied'] = libc.unshare(0x10000000) == -1 and ctypes.get_errno() == errno.EPERM
files = []
try:
    for _ in range(256): files.append(open('/dev/null'))
    r['fd_ceiling'] = False
except OSError as e:
    r['fd_ceiling'] = e.errno == errno.EMFILE
print(json.dumps(r))
"#;
    let out = helper(config, script);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let checks: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        checks.as_object().unwrap().values().all(|v| v == true),
        "{checks}"
    );
}

#[test]
fn helper_memory_and_cpu_limits_stop_bounded_adversarial_workloads() {
    use std::os::unix::process::ExitStatusExt;
    let root = tempfile::tempdir().unwrap();
    let mut memory = spec(root.path(), None);
    memory["limits"]["memory_bytes"] = serde_json::json!(128 << 20);
    let out = helper(
        memory,
        "try:\n x = bytearray(512 << 20)\n print('ESCAPED')\nexcept MemoryError:\n print('memory denied')",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "memory denied");
    let mut cpu = spec(root.path(), None);
    cpu["limits"]["cpu_secs"] = serde_json::json!(1);
    let out = helper(cpu, "while True: pass");
    assert!(matches!(out.status.signal(), Some(24 | 9)));
}

#[tokio::test]
#[ignore = "requires delegated cgroup v2 with memory/pids/cpu controllers and Bubblewrap namespaces"]
async fn cgroup_limits_apply_before_the_first_workload_instruction() -> Result<()> {
    let root = tempfile::tempdir()?;
    let policy = layeragent_config::Policy {
        limits: layeragent_config::Limits {
            cgroup: true,
            max_pids: Some(16),
            memory_bytes: Some(128 << 20),
            cpu_percent: Some(50),
            ..Default::default()
        },
        ..Default::default()
    };
    let out = NativeSandbox::new(env!("CARGO_BIN_EXE_layeragent").into())
        .execute(
            request(
                &[
                    "python3",
                    "-I",
                    "-c",
                    "import pathlib; print(pathlib.Path('/proc/self/cgroup').read_text())",
                ],
                policy,
                root.path(),
            ),
            CancellationToken::new(),
            None,
        )
        .await?;
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    // bwrap's cgroup namespace presents the delegated leaf as its root.
    assert!(out.stdout.contains("0::/"), "{}", out.stdout);
    Ok(())
}

#[tokio::test]
#[ignore = "requires working Bubblewrap network/PID/mount namespaces; certifies the browser outer boundary without downloading Chromium"]
async fn browser_outer_namespace_hides_host_files_and_direct_network() -> Result<()> {
    let profile = tempfile::tempdir()?;
    let host = tempfile::tempdir()?;
    let secret = host.path().join("host-secret");
    std::fs::write(&secret, "private host data")?;
    let proxy = layeragent_sandbox::NetworkProxy::start(vec![], None).await?;
    let code = format!(
        r#"import pathlib,socket,json
r = {{}}
try:
 pathlib.Path({secret:?}).read_text(); r['host_hidden'] = False
except OSError: r['host_hidden'] = True
try:
 socket.create_connection(('1.1.1.1',53),timeout=1); r['network_closed'] = False
except OSError: r['network_closed'] = True
pathlib.Path('/profile/allowed').write_text('isolated profile')
print(json.dumps(r))
"#,
        secret = secret.to_string_lossy()
    );
    let mut command = layeragent_sandbox::browser_command(
        std::path::Path::new(env!("CARGO_BIN_EXE_layeragent")),
        std::path::Path::new("/usr/bin/python3"),
        profile.path(),
        proxy.socket(),
        &["-I".into(), "-c".into(), code],
    )?;
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .kill_on_drop(true);
    let output =
        tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let checks: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(checks["host_hidden"], true);
    assert_eq!(checks["network_closed"], true);
    assert_eq!(
        std::fs::read_to_string(profile.path().join("allowed"))?,
        "isolated profile"
    );
    Ok(())
}
