//! macOS Seatbelt (SBPL) profile generation.
//!
//! The profile is built from the same root/guard model as the Linux backend. Rules avoid
//! depending on SBPL rule ordering: protections are expressed with `require-not` inside
//! the allow rule itself. Generation is pure so it is unit-tested on every platform; only
//! the launch in `lib.rs` is macOS-specific, and kernel enforcement is unvalidated here.
#![cfg_attr(not(test), allow(dead_code))]
use crate::guard::{Guard, GuardKind, RootSpec};
use anyhow::{Result, ensure};
use std::path::Path;

fn quote(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 sandbox path"))?;
    ensure!(
        !text.contains(['\n', '\r', '\0']),
        "unsupported control character in sandbox path"
    );
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}

fn term(path: &Path, dir: bool) -> Result<String> {
    Ok(format!(
        "({} {})",
        if dir { "subpath" } else { "literal" },
        quote(path)?
    ))
}

fn any_of(terms: &[String]) -> String {
    format!("(require-any {})", terms.join(" "))
}

pub(crate) fn profile(
    roots: &[RootSpec],
    guards: &[Guard],
    tmp: &Path,
    proxy_port: Option<u16>,
) -> Result<String> {
    let mut out = String::from(
        "(version 1)\n(deny default)\n\
         (allow process-exec*)\n(allow process-fork)\n(allow signal (target self))\n\
         (allow sysctl-read)\n(allow file-read-metadata)\n\
         (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\") \
         (global-name \"com.apple.logd\") (global-name \"com.apple.system.notification_center\") \
         (global-name \"com.apple.trustd.agent\"))\n\
         (allow file-read* (literal \"/\") (subpath \"/usr\") (subpath \"/bin\") (subpath \"/sbin\") \
         (subpath \"/System\") (subpath \"/Library\") (subpath \"/private/etc\") (subpath \"/dev\") \
         (subpath \"/opt/homebrew\") (subpath \"/Applications\"))\n\
         (allow file-write* (literal \"/dev/null\") (literal \"/dev/dtracehelper\") (literal \"/dev/tty\"))\n",
    );
    let mut readable: Vec<String> = roots
        .iter()
        .map(|r| term(&r.path, true))
        .collect::<Result<_>>()?;
    readable.push(term(tmp, true)?);
    let secrets: Vec<String> = guards
        .iter()
        .filter(|g| g.kind == GuardKind::Secret)
        .map(|g| term(&g.path, g.dir))
        .collect::<Result<_>>()?;
    let frozen: Vec<String> = guards
        .iter()
        .map(|g| term(&g.path, g.dir))
        .collect::<Result<_>>()?;
    let mut writable: Vec<String> = roots
        .iter()
        .filter(|r| r.write)
        .map(|r| term(&r.path, true))
        .collect::<Result<_>>()?;
    writable.push(term(tmp, true)?);
    let guarded = |allowed: &[String], blocked: &[String]| {
        if blocked.is_empty() {
            any_of(allowed)
        } else {
            format!(
                "(require-all {} (require-not {}))",
                any_of(allowed),
                any_of(blocked)
            )
        }
    };
    out.push_str(&format!(
        "(allow file-read* {})\n",
        guarded(&readable, &secrets)
    ));
    out.push_str(&format!(
        "(allow file-write* {})\n",
        guarded(&writable, &frozen)
    ));
    if let Some(port) = proxy_port {
        out.push_str(&format!(
            "(allow network-outbound (remote ip \"localhost:{port}\"))\n"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn root(path: &str, write: bool) -> RootSpec {
        RootSpec {
            path: PathBuf::from(path),
            write,
        }
    }
    fn guard(path: &str, kind: GuardKind, dir: bool) -> Guard {
        Guard {
            path: PathBuf::from(path),
            kind,
            dir,
        }
    }

    #[test]
    fn protections_are_part_of_the_allow_rules_not_order_dependent() {
        let p = profile(
            &[root("/work/ws", true), root("/work/docs", false)],
            &[
                guard("/work/ws/.git", GuardKind::Protected, true),
                guard("/work/ws/.env", GuardKind::Secret, false),
            ],
            Path::new("/private/tmp/la-1"),
            None,
        )
        .unwrap();
        let write = p
            .lines()
            .find(|l| l.starts_with("(allow file-write* (require"))
            .unwrap();
        assert!(write.contains("(subpath \"/work/ws\")"));
        assert!(!write.contains("/work/docs\"))") || write.matches("/work/docs").count() == 0);
        assert!(write.contains(
            "(require-not (require-any (subpath \"/work/ws/.git\") (literal \"/work/ws/.env\")))"
        ));
        let read = p
            .lines()
            .find(|l| l.starts_with("(allow file-read* (require"))
            .unwrap();
        assert!(read.contains("/work/docs"));
        assert!(read.contains("(require-not (require-any (literal \"/work/ws/.env\")))"));
        assert!(!read.contains(".git"), "protected paths stay readable");
        assert!(p.starts_with("(version 1)\n(deny default)"));
        assert!(!p.contains("network"));
    }

    #[test]
    fn network_is_limited_to_the_proxy_port_and_paths_are_escaped() {
        let p = profile(
            &[root("/w\"x", true)],
            &[],
            Path::new("/private/tmp/a"),
            Some(4242),
        )
        .unwrap();
        assert!(p.contains("(allow network-outbound (remote ip \"localhost:4242\"))"));
        assert!(p.contains("\"/w\\\"x\""));
        assert!(profile(&[root("/w\nx", true)], &[], Path::new("/t"), None).is_err());
    }
}
