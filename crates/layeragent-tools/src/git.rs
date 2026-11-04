//! Brokered Git writes.
//!
//! `.git` is read-only inside the sandbox, so staging and committing run here on the
//! host instead, with every code-execution vector Git offers switched off: hooks,
//! fsmonitor, filters, includes, system/global config and prompts. Both operations
//! require explicit approval and never touch secret-looking paths.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

const OUTPUT_LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const MAX_PATHS: usize = 256;

pub mod team;
const MAX_MESSAGE: usize = 8 * 1024;

fn git_dir(workspace: &Path) -> Result<PathBuf> {
    let dir = workspace.join(".git");
    ensure!(
        dir.is_dir() && !dir.is_symlink(),
        "workspace is not a plain Git repository (.git must be a directory)"
    );
    Ok(dir)
}

/// Refuses repositories whose own configuration could run code or move the work tree.
pub(crate) fn audit_config(text: &str) -> Result<()> {
    for line in text.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if let Some(section) = lower.strip_prefix('[') {
            let name = section
                .trim_end_matches(']')
                .split_whitespace()
                .next()
                .unwrap_or("");
            let name = name.split('"').next().unwrap_or(name);
            ensure!(
                !["filter", "include", "includeif"].contains(&name),
                "repository config section [{name}] is not allowed for brokered Git writes"
            );
        } else if lower.starts_with("worktree") || lower.starts_with("gitdir") {
            bail!("repository config redirects the work tree");
        }
    }
    Ok(())
}

/// A path as Git accepts it on the command line. Canonical Windows paths carry
/// the verbatim prefix (`\\?\C:\...`), which Git cannot create directories under.
fn git_arg(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{unc}");
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_owned(),
        _ => text.into_owned(),
    }
}

async fn run(workspace: &Path, args: &[&str]) -> Result<(i32, String)> {
    run_with(workspace, args, &[]).await
}

/// `run` with extra environment, such as a private index file.
async fn run_with(
    workspace: &Path,
    args: &[&str],
    env: &[(&str, String)],
) -> Result<(i32, String)> {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.pager=cat",
            "-c",
            "protocol.ext.allow=never",
        ])
        .args(args)
        .current_dir(workspace)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_ASKPASS", "true")
        .env("LANG", "C.UTF-8")
        .envs(env.iter().map(|(key, value)| (*key, value.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("starting git")?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let work = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut limited_out = (&mut stdout).take(OUTPUT_LIMIT as u64);
        let mut limited_err = (&mut stderr).take(OUTPUT_LIMIT as u64);
        let _ = tokio::join!(
            limited_out.read_to_end(&mut out),
            limited_err.read_to_end(&mut err)
        );
        let status = child.wait().await?;
        out.extend_from_slice(&err);
        Ok::<_, anyhow::Error>((
            status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out).into_owned(),
        ))
    };
    tokio::time::timeout(TIMEOUT, work)
        .await
        .context("git timed out")?
}

fn relative_paths(workspace: &Path, paths: &[String]) -> Result<Vec<String>> {
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_PATHS,
        "provide 1..{MAX_PATHS} explicit paths"
    );
    let root = workspace.canonicalize()?;
    paths
        .iter()
        .map(|p| {
            ensure!(
                !p.starts_with('-') && p != "." && !p.contains(['*', '?', '[']),
                "explicit file paths required: {p}"
            );
            let full = crate::resolve_path(workspace, p, true)?;
            Ok(full.strip_prefix(&root)?.to_string_lossy().into_owned())
        })
        .collect()
}

pub(crate) async fn stage(workspace: &Path, paths: &[String]) -> Result<Value> {
    let dir = git_dir(workspace)?;
    audit_config(&std::fs::read_to_string(dir.join("config")).unwrap_or_default())?;
    let relative = relative_paths(workspace, paths)?;
    let mut args = vec!["add", "-A", "--"];
    args.extend(relative.iter().map(String::as_str));
    let (code, output) = run(workspace, &args).await?;
    Ok(json!({"code":code,"output":output,"staged":relative}))
}

pub(crate) async fn commit(workspace: &Path, message: &str) -> Result<Value> {
    let dir = git_dir(workspace)?;
    audit_config(&std::fs::read_to_string(dir.join("config")).unwrap_or_default())?;
    ensure!(
        !message.trim().is_empty() && message.len() <= MAX_MESSAGE && !message.contains('\0'),
        "commit message must be 1..{MAX_MESSAGE} bytes"
    );
    let (code, names) = run(workspace, &["diff", "--cached", "--name-only", "-z"]).await?;
    ensure!(code == 0, "unable to list staged files: {names}");
    let staged: Vec<&str> = names.split('\0').filter(|n| !n.is_empty()).collect();
    ensure!(!staged.is_empty(), "nothing is staged");
    for name in &staged {
        ensure!(
            !name
                .split('/')
                .any(layeragent_config::is_secret_path_component),
            "refusing to commit sensitive path {name}"
        );
    }
    let identity = run(workspace, &["config", "--get", "user.email"]).await?.1;
    let mut args = Vec::new();
    if identity.trim().is_empty() {
        args.extend([
            "-c",
            "user.name=LayerAgent",
            "-c",
            "user.email=layeragent@localhost",
        ]);
    }
    args.extend(["commit", "--no-verify", "--no-gpg-sign", "-m", message]);
    let (code, output) = run(workspace, &args).await?;
    Ok(json!({"code":code,"output":output,"files":staged.len()}))
}

/// Creates a detached worktree of HEAD so a child agent can write without touching the
/// parent's files. Runs on the host with the same hardened Git invocation as commits.
pub async fn worktree_add(repo: &Path, dest: &Path) -> Result<()> {
    let dir = git_dir(repo)?;
    audit_config(&std::fs::read_to_string(dir.join("config")).unwrap_or_default())?;
    ensure!(!dest.exists(), "worktree destination already exists");
    let dest_text = git_arg(dest);
    let (code, output) = run(repo, &["worktree", "add", "--detach", &dest_text, "HEAD"]).await?;
    ensure!(code == 0, "git worktree add failed: {output}");
    Ok(())
}

/// Everything the child changed relative to HEAD, as a binary-safe patch. The
/// diff goes through a file: captured output is bounded, and a truncated patch
/// would silently drop changes.
pub async fn worktree_patch(worktree: &Path) -> Result<String> {
    let (code, output) = run(worktree, &["add", "-A"]).await?;
    ensure!(code == 0, "git add failed: {output}");
    let file = sibling(worktree, "patch");
    let result = async {
        let output_arg = format!("--output={}", git_arg(&file));
        let (code, error) = run(
            worktree,
            &["diff", "--cached", "--binary", &output_arg, "HEAD"],
        )
        .await?;
        ensure!(code == 0, "git diff failed: {error}");
        Ok(String::from_utf8_lossy(&std::fs::read(&file)?).into_owned())
    }
    .await;
    let _ = std::fs::remove_file(&file);
    result
}

/// A scratch path next to `dir`, outside the tree Git is looking at.
fn sibling(dir: &Path, extension: &str) -> PathBuf {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "worktree".into());
    dir.with_file_name(format!(".{name}.{}.{extension}", std::process::id()))
}

pub async fn worktree_remove(repo: &Path, dest: &Path) {
    let dest_text = git_arg(dest);
    let _ = run(repo, &["worktree", "remove", "--force", &dest_text]).await;
    let _ = std::fs::remove_dir_all(dest);
    let _ = run(repo, &["worktree", "prune"]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_windows_paths_are_passed_to_git_in_plain_form() {
        assert_eq!(
            git_arg(Path::new(r"\\?\C:\Temp\agents\x")),
            r"C:\Temp\agents\x"
        );
        assert_eq!(
            git_arg(Path::new(r"\\?\UNC\server\share\x")),
            r"\\server\share\x"
        );
        assert_eq!(git_arg(Path::new("/var/state/x")), "/var/state/x");
        assert_eq!(git_arg(Path::new(r"\\?\Volume{1}\x")), r"\\?\Volume{1}\x");
    }

    #[test]
    fn config_audit_rejects_code_execution_vectors() {
        assert!(audit_config("[core]\n\tbare = false\n[user]\n\tname = a").is_ok());
        assert!(audit_config("[filter \"lfs\"]\n\tclean = evil").is_err());
        assert!(audit_config("[include]\n\tpath = /x").is_err());
        assert!(audit_config("[includeIf \"gitdir:/\"]\n\tpath = /x").is_err());
        assert!(audit_config("[core]\n\tworktree = /etc").is_err());
    }

    async fn sh(dir: &Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .await
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn stages_and_commits_explicit_paths_but_never_secrets_or_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        sh(ws, &["init", "-q"]).await;
        let marker = ws.join("hook-ran");
        let hook = ws.join(".git/hooks/pre-commit");
        std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", marker.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(ws.join("a.txt"), "hello").unwrap();
        std::fs::write(ws.join(".env"), "SECRET=1").unwrap();

        assert!(stage(ws, &[".env".into()]).await.is_err());
        assert!(stage(ws, &[".".into()]).await.is_err());
        assert!(stage(ws, &[".git/config".into()]).await.is_err());
        assert!(commit(ws, "empty").await.is_err());

        assert_eq!(stage(ws, &["a.txt".into()]).await.unwrap()["code"], 0);
        let committed = commit(ws, "add a").await.unwrap();
        assert_eq!(committed["code"], 0, "{committed}");
        assert!(!marker.exists(), "hooks must not run");
        let log = tokio::process::Command::new("git")
            .args(["log", "--format=%s"])
            .current_dir(ws)
            .output()
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "add a");
    }

    #[tokio::test]
    async fn worktree_isolates_child_edits_and_reports_a_patch() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, &["init", "-q"]).await;
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        stage(&repo, &["a.txt".into()]).await.unwrap();
        assert_eq!(commit(&repo, "base").await.unwrap()["code"], 0);
        let wt = dir.path().join("wt");
        worktree_add(&repo, &wt).await.unwrap();
        std::fs::write(wt.join("a.txt"), "two\n").unwrap();
        std::fs::write(wt.join("new.txt"), "n\n").unwrap();
        let patch = worktree_patch(&wt).await.unwrap();
        assert!(
            patch.contains("+two") && patch.contains("new.txt"),
            "{patch}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "one\n"
        );
        // Larger than the captured-output bound: nothing may be cut off.
        let big: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(wt.join("big.txt"), &big).unwrap();
        let patch = worktree_patch(&wt).await.unwrap();
        assert!(patch.len() > OUTPUT_LIMIT, "{}", patch.len());
        assert!(patch.contains("+line 19999"));
        worktree_remove(&repo, &wt).await;
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn hostile_repository_config_blocks_the_broker() {
        let dir = tempfile::tempdir().unwrap();
        sh(dir.path(), &["init", "-q"]).await;
        let config = dir.path().join(".git/config");
        let mut text = std::fs::read_to_string(&config).unwrap();
        text.push_str("[filter \"x\"]\n\tclean = touch pwned\n");
        std::fs::write(&config, text).unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(stage(dir.path(), &["a.txt".into()]).await.is_err());
        assert!(!dir.path().join("pwned").exists());
    }
}
