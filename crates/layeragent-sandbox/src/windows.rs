//! Windows backend.
//!
//! The helper process (`__sandbox_inner`) builds a restricted primary token with
//! `WRITE_RESTRICTED` and the `RESTRICTED` SID as its only restricting SID, so writes
//! succeed only where an ACL names `RESTRICTED`. The parent grants that SID modify rights
//! on the write roots (and denies it on protected paths) for the duration of a run, and
//! the helper places the workload in a Job Object with `KILL_ON_JOB_CLOSE` plus process,
//! memory and CPU-time ceilings, which also terminates the whole process tree.
//!
//! Known gaps, reported by `doctor`: reads are not restricted (secrets are only hidden
//! from file tools), there is no network, and no AppContainer. Execution therefore stays
//! disabled in production until those gaps are closed and certified on Windows.
#![cfg_attr(not(test), allow(dead_code))]
use crate::guard::{Guard, GuardKind, RootSpec};
use std::path::{Path, PathBuf};

pub(crate) const RESTRICTED_SID: &str = "*S-1-5-12";

/// Quotes one argument following the MSVCRT/CommandLineToArgvW rules.
pub(crate) fn quote_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\u{b}', '"']) {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.push_str(&"\\".repeat(backslashes * 2 + 1));
                out.push('"');
                backslashes = 0;
            }
            _ => {
                out.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.push_str(&"\\".repeat(backslashes * 2));
    out.push('"');
    out
}

pub(crate) fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One `icacls` invocation that is applied for a run and undone afterwards.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AclChange {
    pub path: PathBuf,
    pub apply: Vec<String>,
    pub revoke: Vec<String>,
}

fn inherit(dir: bool) -> &'static str {
    if dir { "(OI)(CI)" } else { "" }
}

/// ACL edits for a run: modify rights on write roots, a write/delete deny on every guard.
pub(crate) fn acl_plan(roots: &[RootSpec], guards: &[Guard], tmp: &Path) -> Vec<AclChange> {
    let mut changes = Vec::new();
    let mut grant = |path: &Path| {
        changes.push(AclChange {
            path: path.to_path_buf(),
            apply: vec![
                path.to_string_lossy().into_owned(),
                "/grant".into(),
                format!("{RESTRICTED_SID}:(OI)(CI)M"),
            ],
            revoke: vec![
                path.to_string_lossy().into_owned(),
                "/remove:g".into(),
                RESTRICTED_SID.into(),
            ],
        });
    };
    for root in roots.iter().filter(|r| r.write) {
        grant(&root.path);
    }
    grant(tmp);
    for guard in guards {
        let _ = guard.kind == GuardKind::Secret; // reads cannot be denied, writes always are
        changes.push(AclChange {
            path: guard.path.clone(),
            apply: vec![
                guard.path.to_string_lossy().into_owned(),
                "/deny".into(),
                format!("{RESTRICTED_SID}:{}(W,D,DC)", inherit(guard.dir)),
            ],
            revoke: vec![
                guard.path.to_string_lossy().into_owned(),
                "/remove:d".into(),
                RESTRICTED_SID.into(),
            ],
        });
    }
    changes
}

#[cfg(windows)]
pub use native::inner;

#[cfg(windows)]
mod native {
    use super::*;
    use crate::InnerSpec;
    use anyhow::{Result, bail, ensure};
    use layeragent_config::Limits;
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
    };
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, LocalFree},
        Security::{
            Authorization::ConvertStringSidToSidW, CreateRestrictedToken, DISABLE_MAX_PRIVILEGE,
            SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
            TOKEN_QUERY, WRITE_RESTRICTED,
        },
        System::{
            Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
                JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
                JOBOBJECT_BASIC_UI_RESTRICTIONS, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JobObjectBasicUIRestrictions, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessAsUserW, GetCurrentProcess,
                GetExitCodeProcess, INFINITE, OpenProcessToken, PROCESS_INFORMATION, ResumeThread,
                STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
            },
        },
    };

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(what: &str) -> anyhow::Error {
        anyhow::anyhow!("{what} failed: {}", std::io::Error::last_os_error())
    }

    /// Refcounted ACL edits, so concurrent runs sharing a root do not revoke each other.
    pub(crate) struct AclScope {
        changes: Vec<AclChange>,
    }

    fn counts() -> &'static Mutex<HashMap<(PathBuf, Vec<String>), usize>> {
        static COUNTS: OnceLock<Mutex<HashMap<(PathBuf, Vec<String>), usize>>> = OnceLock::new();
        COUNTS.get_or_init(Default::default)
    }

    fn icacls(args: &[String]) -> Result<()> {
        let output = std::process::Command::new("icacls").args(args).output()?;
        ensure!(
            output.status.success(),
            "icacls {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stdout)
        );
        Ok(())
    }

    impl AclScope {
        pub(crate) fn apply(changes: Vec<AclChange>) -> Result<Self> {
            let mut applied = Vec::new();
            for change in changes {
                let key = (change.path.clone(), change.apply.clone());
                let first = {
                    let mut map = counts().lock().map_err(|_| anyhow::anyhow!("acl lock"))?;
                    let n = map.entry(key.clone()).or_insert(0);
                    *n += 1;
                    *n == 1
                };
                if first && let Err(e) = icacls(&change.apply) {
                    let _ = counts().lock().map(|mut m| m.remove(&key));
                    drop(Self { changes: applied });
                    return Err(e);
                }
                applied.push(change);
            }
            Ok(Self { changes: applied })
        }
    }

    impl Drop for AclScope {
        fn drop(&mut self) {
            for change in self.changes.drain(..).rev() {
                let key = (change.path.clone(), change.apply.clone());
                let last = counts()
                    .lock()
                    .map(|mut map| match map.get_mut(&key) {
                        Some(n) if *n > 1 => {
                            *n -= 1;
                            false
                        }
                        _ => {
                            map.remove(&key);
                            true
                        }
                    })
                    .unwrap_or(true);
                if last {
                    let _ = icacls(&change.revoke);
                }
            }
        }
    }

    fn job(limits: &Limits) -> Result<HANDLE> {
        // SAFETY: plain Win32 calls with properly initialised, correctly sized structures.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(last_error("CreateJobObject"));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            let flags = &mut info.BasicLimitInformation.LimitFlags;
            *flags |=
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
            if let Some(n) = limits.max_pids {
                *flags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
                info.BasicLimitInformation.ActiveProcessLimit = n.min(u32::MAX as u64) as u32;
            }
            if let Some(bytes) = limits.memory_bytes {
                *flags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
                info.ProcessMemoryLimit = bytes as usize;
            }
            if let Some(secs) = limits.cpu_secs {
                *flags |= JOB_OBJECT_LIMIT_PROCESS_TIME;
                // 100 ns units.
                info.BasicLimitInformation.PerProcessUserTimeLimit = (secs * 10_000_000) as i64;
            }
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            ) == 0
            {
                return Err(last_error("SetInformationJobObject(limits)"));
            }
            let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
                UIRestrictionsClass: 0xFF, // all JOB_OBJECT_UILIMIT_* flags
            };
            if SetInformationJobObject(
                job,
                JobObjectBasicUIRestrictions,
                (&raw const ui).cast(),
                std::mem::size_of_val(&ui) as u32,
            ) == 0
            {
                return Err(last_error("SetInformationJobObject(ui)"));
            }
            Ok(job)
        }
    }

    fn restricted_token() -> Result<HANDLE> {
        // SAFETY: handles are closed on every path; the SID is freed after use.
        unsafe {
            let mut current: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
                &mut current,
            ) == 0
            {
                return Err(last_error("OpenProcessToken"));
            }
            let mut sid = std::ptr::null_mut();
            if ConvertStringSidToSidW(wide("S-1-5-12").as_ptr(), &mut sid) == 0 {
                CloseHandle(current);
                return Err(last_error("ConvertStringSidToSid"));
            }
            let restricting = SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0,
            };
            let mut restricted: HANDLE = std::ptr::null_mut();
            let ok = CreateRestrictedToken(
                current,
                DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
                1,
                &restricting,
                &mut restricted,
            );
            LocalFree(sid.cast());
            CloseHandle(current);
            if ok == 0 {
                return Err(last_error("CreateRestrictedToken"));
            }
            Ok(restricted)
        }
    }

    /// Runs the workload under the restricted token inside the job, then exits with its code.
    pub fn inner(spec: &InnerSpec, argv: &[String]) -> Result<()> {
        ensure!(!argv.is_empty(), "empty inner command");
        ensure!(
            spec.proxy_port.is_none(),
            "network is unavailable on Windows"
        );
        let job = job(&spec.limits)?;
        let token = restricted_token()?;
        let mut command_line = wide(&super::command_line(argv));
        // SAFETY: every pointer refers to live, correctly initialised memory; the child is
        // created suspended so it cannot run before it is inside the job.
        unsafe {
            let mut startup: STARTUPINFOW = std::mem::zeroed();
            startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            startup.dwFlags = STARTF_USESTDHANDLES;
            startup.hStdInput = GetStdHandle(STD_INPUT_HANDLE);
            startup.hStdOutput = GetStdHandle(STD_OUTPUT_HANDLE);
            startup.hStdError = GetStdHandle(STD_ERROR_HANDLE);
            let mut process: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessAsUserW(
                token,
                std::ptr::null(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_SUSPENDED | CREATE_NO_WINDOW,
                std::ptr::null(),
                std::ptr::null(),
                &startup,
                &mut process,
            ) == 0
            {
                return Err(last_error("CreateProcessAsUser"));
            }
            CloseHandle(token);
            if AssignProcessToJobObject(job, process.hProcess) == 0 {
                let error = last_error("AssignProcessToJobObject");
                windows_sys::Win32::System::Threading::TerminateProcess(process.hProcess, 1);
                return Err(error);
            }
            if ResumeThread(process.hThread) == u32::MAX {
                bail!("{}", last_error("ResumeThread"));
            }
            WaitForSingleObject(process.hProcess, INFINITE);
            let mut code = 1u32;
            GetExitCodeProcess(process.hProcess, &mut code);
            CloseHandle(process.hThread);
            CloseHandle(process.hProcess);
            // Closing the job (process exit) terminates any descendants.
            std::process::exit(code as i32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_quoted_for_createprocess() {
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(
            quote_arg("C:\\dir with space\\"),
            "\"C:\\dir with space\\\\\""
        );
        assert_eq!(
            command_line(&["python".into(), "-c".into(), "print('x y')".into()]),
            "python -c \"print('x y')\""
        );
    }

    #[test]
    fn acl_plan_grants_write_roots_and_denies_guards_reversibly() {
        let roots = [
            RootSpec {
                path: "C:/ws".into(),
                write: true,
            },
            RootSpec {
                path: "C:/docs".into(),
                write: false,
            },
        ];
        let guards = [
            Guard {
                path: "C:/ws/.git".into(),
                kind: GuardKind::Protected,
                dir: true,
            },
            Guard {
                path: "C:/ws/.env".into(),
                kind: GuardKind::Secret,
                dir: false,
            },
        ];
        let plan = acl_plan(&roots, &guards, Path::new("C:/tmp/run"));
        assert_eq!(plan.len(), 4);
        assert!(plan[0].apply.contains(&"/grant".to_string()));
        assert!(plan.iter().all(|c| c.path != Path::new("C:/docs")));
        assert!(plan[2].apply[2].ends_with("(OI)(CI)(W,D,DC)"));
        assert!(
            plan[3].apply[2].ends_with(":(W,D,DC)"),
            "files take no inheritance flags"
        );
        assert!(plan[2].revoke.contains(&"/remove:d".to_string()));
    }
}
