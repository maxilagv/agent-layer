//! Bubblewrap argument planning. Pure with respect to process state so the
//! resulting mount layout can be unit-tested without namespaces.
use crate::{
    InnerSpec,
    guard::{GuardKind, resolve_roots, scan_guards},
};
use anyhow::Result;
use layeragent_config::Policy;
use std::{ffi::OsString, path::Path};

pub(crate) const PROXY_SOCKET_IN_SANDBOX: &str = "/run/layeragent-proxy.sock";

pub(crate) struct Plan {
    pub args: Vec<OsString>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub spec: InnerSpec,
}

pub(crate) fn plan(
    helper: &Path,
    workspace: &Path,
    policy: &Policy,
    argv: &[String],
    proxy_socket: Option<&Path>,
) -> Result<Plan> {
    let roots = resolve_roots(workspace, policy)?;
    let workspace = roots
        .iter()
        .find(|r| r.path == workspace.canonicalize().unwrap_or_default())
        .map_or_else(|| workspace.to_path_buf(), |r| r.path.clone());
    let guards = scan_guards(&roots, policy)?;
    let mut args: Vec<OsString> = [
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
    ]
    .map(OsString::from)
    .into();
    for path in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/etc/alternatives",
        "/etc/ssl",
        "/etc/passwd",
        "/etc/group",
    ] {
        if Path::new(path).exists() {
            args.extend(["--ro-bind".into(), path.into(), path.into()]);
        }
    }
    for root in &roots {
        args.extend([
            if root.write { "--bind" } else { "--ro-bind" }.into(),
            root.path.clone().into(),
            root.path.clone().into(),
        ]);
    }
    for guard in &guards {
        match (guard.kind, guard.dir) {
            (GuardKind::Secret, true) => args.extend([
                "--tmpfs".into(),
                guard.path.clone().into(),
                "--remount-ro".into(),
                guard.path.clone().into(),
            ]),
            (GuardKind::Secret, false) => args.extend([
                "--ro-bind".into(),
                "/dev/null".into(),
                guard.path.clone().into(),
            ]),
            (GuardKind::Protected, _) => args.extend([
                "--ro-bind".into(),
                guard.path.clone().into(),
                guard.path.clone().into(),
            ]),
        }
    }
    let mut spec = InnerSpec {
        write_roots: roots
            .iter()
            .filter(|root| root.write)
            .map(|root| root.path.clone())
            .collect(),
        limits: policy.limits.clone(),
        proxy_socket: None,
        proxy_port: None,
    };
    if policy.network {
        let host_socket = proxy_socket
            .ok_or_else(|| anyhow::anyhow!("network policy requires the confined proxy"))?;
        args.extend([
            "--bind".into(),
            host_socket.into(),
            PROXY_SOCKET_IN_SANDBOX.into(),
        ]);
        spec.proxy_socket = Some(PROXY_SOCKET_IN_SANDBOX.into());
    }
    args.extend([
        "--ro-bind".into(),
        helper.into(),
        "/layeragent-helper".into(),
    ]);
    args.extend(["--chdir".into(), workspace.into(), "--".into()]);
    args.extend([
        "/layeragent-helper".into(),
        "__sandbox_inner".into(),
        serde_json::to_string(&spec)?.into(),
        "--".into(),
    ]);
    args.extend(argv.iter().map(OsString::from));
    Ok(Plan { args, spec })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(plan: &Plan) -> Vec<String> {
        plan.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
    fn has_pair(args: &[String], flag: &str, path: &Path) -> bool {
        let path = path.to_string_lossy();
        args.windows(3)
            .any(|w| w[0] == flag && w[1] == path && w[2] == path)
    }

    #[test]
    fn multiple_roots_get_the_right_modes_and_protections() {
        let ws = tempfile::tempdir().unwrap();
        let docs = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        std::fs::create_dir(ws.path().join(".git")).unwrap();
        std::fs::create_dir_all(docs.path().join("private")).unwrap();
        std::fs::write(docs.path().join("private/key.pem"), "x").unwrap();
        std::fs::write(cache.path().join(".env"), "SECRET").unwrap();
        let policy = Policy {
            read_roots: vec![docs.path().into()],
            write_roots: vec![cache.path().into()],
            protected_paths: vec!["private".into()],
            ..Policy::default()
        };
        let p = plan(Path::new("/h"), ws.path(), &policy, &["true".into()], None).unwrap();
        let args = rendered(&p);
        let (ws_c, docs_c, cache_c) = (
            ws.path().canonicalize().unwrap(),
            docs.path().canonicalize().unwrap(),
            cache.path().canonicalize().unwrap(),
        );
        assert!(has_pair(&args, "--bind", &ws_c));
        assert!(has_pair(&args, "--ro-bind", &docs_c));
        assert!(has_pair(&args, "--bind", &cache_c));
        assert!(has_pair(&args, "--ro-bind", &ws_c.join(".git")));
        assert!(has_pair(&args, "--ro-bind", &docs_c.join("private")));
        assert!(args.windows(3).any(|w| w[0] == "--ro-bind"
            && w[1] == "/dev/null"
            && w[2] == cache_c.join(".env").to_string_lossy()));
        assert_eq!(p.spec.write_roots, vec![ws_c.clone(), cache_c.clone()]);
        // Read-only mounts precede writable ones.
        let ro = args
            .iter()
            .position(|a| *a == docs_c.to_string_lossy())
            .unwrap();
        let rw = args
            .iter()
            .position(|a| *a == cache_c.to_string_lossy())
            .unwrap();
        assert!(ro < rw);
    }

    #[test]
    fn read_only_policy_has_no_workspace_write_root() {
        let ws = tempfile::tempdir().unwrap();
        let policy = Policy {
            write: false,
            ..Policy::default()
        };
        let p = plan(Path::new("/h"), ws.path(), &policy, &["true".into()], None).unwrap();
        assert!(p.spec.write_roots.is_empty());
    }

    #[test]
    fn network_requires_proxy_and_binds_its_socket() {
        let ws = tempfile::tempdir().unwrap();
        let policy = Policy {
            network: true,
            allow_domains: vec!["example.com".into()],
            ..Policy::default()
        };
        assert!(plan(Path::new("/h"), ws.path(), &policy, &["true".into()], None).is_err());
        let p = plan(
            Path::new("/h"),
            ws.path(),
            &policy,
            &["true".into()],
            Some(Path::new("/tmp/x.sock")),
        )
        .unwrap();
        assert_eq!(
            p.spec.proxy_socket.as_deref(),
            Some(Path::new(PROXY_SOCKET_IN_SANDBOX))
        );
        assert!(rendered(&p).iter().any(|a| a == "--unshare-all"));
    }

    #[test]
    fn special_filesystems_and_filesystem_root_are_rejected() {
        let ws = tempfile::tempdir().unwrap();
        for bad in ["/", "/proc", "/sys"] {
            let policy = Policy {
                read_roots: vec![bad.into()],
                ..Policy::default()
            };
            assert!(plan(Path::new("/h"), ws.path(), &policy, &["true".into()], None).is_err());
        }
    }
}
