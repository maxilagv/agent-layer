//! The in-namespace helper stage: Landlock, resource limits, seccomp and the
//! loopback bridge to the host-side network proxy.
#[cfg(not(windows))]
use crate::InnerSpec;
#[cfg(not(windows))]
use anyhow::Result;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use anyhow::bail;
#[cfg(unix)]
use anyhow::ensure;
#[cfg(unix)]
use layeragent_config::Limits;
#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
pub(crate) const PROXY_PORT: u16 = 3128;

#[cfg(target_os = "linux")]
pub fn inner(spec: &InnerSpec, argv: &[String]) -> Result<()> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    ensure!(!argv.is_empty(), "empty inner command");
    let Some(socket) = spec.proxy_socket.as_deref() else {
        confine(spec)?;
        let error = std::process::Command::new(&argv[0]).args(&argv[1..]).exec();
        return Err(error.into());
    };
    // Bind before forking so the child can connect immediately; no threads exist yet,
    // which keeps the post-fork confinement free of allocator-lock hazards.
    let listener = std::net::TcpListener::bind(("127.0.0.1", PROXY_PORT))?;
    let proxy = format!("http://127.0.0.1:{PROXY_PORT}");
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
    ] {
        command.env(key, &proxy);
    }
    command.env_remove("NO_PROXY").env_remove("no_proxy");
    let child_spec = spec.clone();
    // SAFETY: runs in the single-threaded forked child before exec.
    unsafe {
        command.pre_exec(move || {
            confine(&child_spec).map_err(|e| std::io::Error::other(e.to_string()))
        });
    }
    let mut child = command.spawn()?;
    let socket = socket.to_path_buf();
    std::thread::spawn(move || bridge(listener, &socket));
    let status = child.wait()?;
    std::process::exit(
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
    );
}

/// Forwards loopback TCP connections to the host proxy over its UNIX socket.
#[cfg(target_os = "linux")]
fn bridge(listener: std::net::TcpListener, socket: &Path) {
    use std::os::unix::net::UnixStream;
    for connection in listener.incoming().flatten() {
        if let Ok(upstream) = UnixStream::connect(socket) {
            super::browser::relay(connection, upstream);
        }
    }
}

#[cfg(unix)]
pub(crate) fn apply_rlimits(limits: &Limits) -> Result<()> {
    fn set(resource: i32, soft: u64, hard: u64) -> Result<()> {
        let limit = libc::rlimit {
            rlim_cur: soft as libc::rlim_t,
            rlim_max: hard as libc::rlim_t,
        };
        // SAFETY: plain syscall with a valid pointer.
        ensure!(
            unsafe { libc::setrlimit(resource as _, &limit) } == 0,
            "setrlimit failed: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }
    set(libc::RLIMIT_CORE as i32, 0, 0)?;
    if let Some(bytes) = limits.memory_bytes {
        set(libc::RLIMIT_AS as i32, bytes, bytes)?;
    }
    if let Some(n) = limits.max_pids {
        set(libc::RLIMIT_NPROC as i32, n, n)?;
    }
    if let Some(secs) = limits.cpu_secs {
        set(libc::RLIMIT_CPU as i32, secs, secs.saturating_add(2))?;
    }
    if let Some(bytes) = limits.file_size_bytes {
        set(libc::RLIMIT_FSIZE as i32, bytes, bytes)?;
    }
    if let Some(n) = limits.open_files {
        set(libc::RLIMIT_NOFILE as i32, n, n)?;
    }
    Ok(())
}

/// Applies Landlock, resource limits and seccomp to the calling (single) thread.
#[cfg(target_os = "linux")]
pub(crate) fn confine(spec: &InnerSpec) -> Result<()> {
    use landlock::{
        ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetStatus,
    };
    use std::collections::BTreeMap;
    apply_rlimits(&spec.limits)?;
    let abi = ABI::V3;
    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))?
        .create()?
        .add_rule(PathBeneath::new(
            PathFd::new("/")?,
            AccessFs::from_read(abi),
        ))?
        .add_rule(PathBeneath::new(
            PathFd::new("/tmp")?,
            AccessFs::from_all(abi),
        ))?;
    for root in &spec.write_roots {
        ruleset = ruleset.add_rule(PathBeneath::new(
            PathFd::new(root)?,
            AccessFs::from_all(abi),
        ))?;
    }
    ensure!(
        ruleset.restrict_self()?.ruleset == RulesetStatus::FullyEnforced,
        "sandbox enforcement failed: Landlock unavailable"
    );
    let mut rules = BTreeMap::new();
    for syscall in [
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_bpf,
        libc::SYS_perf_event_open,
        libc::SYS_open_tree,
        libc::SYS_move_mount,
        libc::SYS_fsopen,
        libc::SYS_fsconfig,
        libc::SYS_fsmount,
        libc::SYS_mount_setattr,
        libc::SYS_userfaultfd,
        libc::SYS_io_uring_setup,
        libc::SYS_keyctl,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_kexec_load,
        libc::SYS_reboot,
    ] {
        rules.insert(syscall, vec![]);
    }
    use seccompiler::{SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompRule};
    // clone3 arguments live behind a pointer and cannot be inspected by classic BPF.
    // ENOSYS lets libc use clone for ordinary threads; namespace flags remain denied.
    let clone3_filter: seccompiler::BpfProgram = seccompiler::SeccompFilter::new(
        BTreeMap::from([(libc::SYS_clone3, vec![])]),
        seccompiler::SeccompAction::Allow,
        seccompiler::SeccompAction::Errno(libc::ENOSYS as u32),
        std::env::consts::ARCH.try_into()?,
    )?
    .try_into()?;
    seccompiler::apply_filter(&clone3_filter)?;
    rules.insert(
        libc::SYS_clone,
        [
            libc::CLONE_NEWUSER,
            libc::CLONE_NEWNS,
            libc::CLONE_NEWPID,
            libc::CLONE_NEWNET,
            libc::CLONE_NEWIPC,
            libc::CLONE_NEWUTS,
            libc::CLONE_NEWCGROUP,
        ]
        .into_iter()
        .map(|flag| {
            SeccompRule::new(vec![SeccompCondition::new(
                0,
                SeccompCmpArgLen::Qword,
                SeccompCmpOp::MaskedEq(flag as u64),
                flag as u64,
            )?])
        })
        .collect::<std::result::Result<Vec<_>, _>>()?,
    );
    // AF_UNIX stays blocked for the workload: the proxy bridge lives in the helper
    // process, which is never filtered, so the workload can only reach loopback TCP.
    let socket_rule = |index, op, value: u64| -> Result<SeccompRule> {
        Ok(SeccompRule::new(vec![SeccompCondition::new(
            index,
            SeccompCmpArgLen::Dword,
            op,
            value,
        )?])?)
    };
    rules.insert(
        libc::SYS_socket,
        vec![
            socket_rule(1, SeccompCmpOp::MaskedEq(15), libc::SOCK_RAW as u64)?,
            socket_rule(0, SeccompCmpOp::Eq, libc::AF_PACKET as u64)?,
            socket_rule(0, SeccompCmpOp::Eq, libc::AF_UNIX as u64)?,
        ],
    );
    let filter: seccompiler::BpfProgram = seccompiler::SeccompFilter::new(
        rules,
        seccompiler::SeccompAction::Allow,
        seccompiler::SeccompAction::Errno(libc::EPERM as u32),
        std::env::consts::ARCH.try_into()?,
    )?
    .try_into()?;
    seccompiler::apply_filter(&filter)?;
    Ok(())
}

/// macOS: Seatbelt is applied by the parent via `sandbox-exec`; this stage only adds
/// resource limits and the proxy environment before replacing itself with the workload.
#[cfg(target_os = "macos")]
pub fn inner(spec: &InnerSpec, argv: &[String]) -> Result<()> {
    use std::os::unix::process::CommandExt;
    ensure!(!argv.is_empty(), "empty inner command");
    apply_rlimits(&spec.limits)?;
    let mut command = std::process::Command::new(&argv[0]);
    command.args(&argv[1..]);
    if let Some(port) = spec.proxy_port {
        let proxy = format!("http://127.0.0.1:{port}");
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "http_proxy",
            "https_proxy",
            "ALL_PROXY",
        ] {
            command.env(key, &proxy);
        }
    }
    Err(command.exec().into())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn inner(_spec: &InnerSpec, _argv: &[String]) -> Result<()> {
    bail!("native helper is unavailable on this platform")
}
