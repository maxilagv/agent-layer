//! Git plumbing for cooperative writers. Each writing member gets a linked
//! worktree whose base commit captures the team workspace as it was (HEAD plus
//! uncommitted and untracked changes), so its patch is the member's own work
//! only. Every command uses the hardened broker invocation, never touches the
//! operator's index, and passes diffs through files so nothing is truncated.
use super::{audit_config, git_arg, git_dir, run, run_with};
use anyhow::{Context, Result, bail, ensure};
use std::path::{Path, PathBuf};

const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=LayerAgent",
    "-c",
    "user.email=layeragent@localhost",
];

/// Most files one team patch may touch.
pub const MAX_PATCH_FILES: usize = 256;

/// A repository-relative path a team patch may carry: inside the tree, not Git
/// metadata and not a secret-looking file.
pub fn check_patch_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.contains('\0'),
        "invalid patch path {path:?}"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "patch path {path:?} must be a plain relative path"
        );
        ensure!(
            !part.eq_ignore_ascii_case(".git"),
            "patch path {path:?} touches Git metadata"
        );
        ensure!(
            !layeragent_config::is_secret_path_component(part),
            "patch path {path:?} looks like a secret"
        );
    }
    Ok(())
}

fn ensure_repository(repo: &Path) -> Result<()> {
    let dir = git_dir(repo)?;
    audit_config(&std::fs::read_to_string(dir.join("config")).unwrap_or_default())
}

/// Refuses to reset or clean anything that is not a linked worktree: a wrong
/// path must never cost the operator their files.
fn ensure_linked(worktree: &Path) -> Result<()> {
    let marker = worktree.join(".git");
    ensure!(
        marker.is_file() && !marker.is_symlink(),
        "not a linked team worktree: {}",
        worktree.display()
    );
    Ok(())
}

async fn checked(dir: &Path, args: &[&str]) -> Result<String> {
    let (code, output) = run(dir, args).await?;
    ensure!(code == 0, "git {} failed: {output}", args[0]);
    Ok(output)
}

fn output_arg(file: &Path) -> String {
    format!("--output={}", git_arg(file))
}

fn read_names(file: &Path) -> Result<Vec<String>> {
    let raw = std::fs::read(file)?;
    let names = String::from_utf8(raw).context("patch file names must be UTF-8")?;
    Ok(names
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect())
}

async fn head(dir: &Path) -> Result<String> {
    Ok(checked(dir, &["rev-parse", "--verify", "HEAD^{commit}"])
        .await?
        .trim()
        .to_string())
}

/// Writes the repository's uncommitted state (tracked changes and untracked,
/// non-ignored files) relative to HEAD into `out`, using a private index so the
/// operator's staging area is untouched. Secret-looking paths are left out.
async fn workspace_state(repo: &Path, scratch: &Path, out: &Path) -> Result<()> {
    let index = scratch.join(format!("index-{}", layeragent_protocol::new_id()));
    let result = async {
        let env = [("GIT_INDEX_FILE", git_arg(&index))];
        let step = |args: &'static [&'static str]| {
            let env = &env;
            async move {
                let (code, output) = run_with(repo, args, env).await?;
                ensure!(code == 0, "git {} failed: {output}", args[0]);
                Ok::<_, anyhow::Error>(())
            }
        };
        step(&["read-tree", "HEAD"]).await?;
        step(&["add", "-A"]).await?;
        let names = scratch.join(format!("names-{}", layeragent_protocol::new_id()));
        let listed = run_with(
            repo,
            &[
                "diff",
                "--cached",
                "--name-only",
                "--no-renames",
                "-z",
                &output_arg(&names),
                "HEAD",
            ],
            &env,
        )
        .await;
        let secrets: Vec<String> = read_names(&names)
            .unwrap_or_default()
            .into_iter()
            .filter(|name| check_patch_path(name).is_err())
            .collect();
        let _ = std::fs::remove_file(&names);
        ensure!(listed?.0 == 0, "unable to list workspace changes");
        for chunk in secrets.chunks(64) {
            let mut args = vec!["rm", "--cached", "-q", "--ignore-unmatch", "--"];
            args.extend(chunk.iter().map(String::as_str));
            let (code, output) = run_with(repo, &args, &env).await?;
            ensure!(code == 0, "git rm failed: {output}");
        }
        let (code, output) = run_with(
            repo,
            &[
                "diff",
                "--cached",
                "--binary",
                "--no-renames",
                &output_arg(out),
                "HEAD",
            ],
            &env,
        )
        .await?;
        ensure!(code == 0, "git diff failed: {output}");
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&index);
    result
}

/// Applies `patch` if it is not empty. With `three_way`, conflicts are left as
/// markers and returned instead of failing.
async fn apply_into(dir: &Path, patch: &Path, three_way: bool) -> Result<Vec<String>> {
    if std::fs::metadata(patch)?.len() == 0 {
        return Ok(vec![]);
    }
    let file = git_arg(patch);
    let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
    if three_way {
        args.push("--3way");
    }
    args.push(&file);
    let (code, output) = run(dir, &args).await?;
    if code == 0 {
        return Ok(vec![]);
    }
    ensure!(three_way, "git apply failed: {output}");
    let conflicted = checked(dir, &["diff", "--name-only", "--diff-filter=U"]).await?;
    let conflicts: Vec<String> = conflicted.lines().map(str::to_string).collect();
    ensure!(!conflicts.is_empty(), "git apply failed: {output}");
    Ok(conflicts)
}

/// Commits everything in the worktree as its new base and returns the commit.
async fn commit_base(worktree: &Path, message: &str) -> Result<String> {
    checked(worktree, &["add", "-A"]).await?;
    let mut args = IDENTITY.to_vec();
    args.extend([
        "commit",
        "--no-verify",
        "--no-gpg-sign",
        "--allow-empty",
        "-q",
        "-m",
        message,
    ]);
    checked(worktree, &args).await?;
    head(worktree).await
}

/// Creates a linked worktree at `dest` holding the team workspace exactly as it
/// is now, committed as the member's base. Returns the base commit.
pub async fn create(repo: &Path, dest: &Path, scratch: &Path) -> Result<String> {
    ensure_repository(repo)?;
    ensure!(!dest.exists(), "worktree destination already exists");
    head(repo)
        .await
        .context("write access needs a repository with at least one commit")?;
    std::fs::create_dir_all(scratch)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    super::worktree_add(repo, dest).await?;
    let result = async {
        let state = scratch.join(format!("state-{}.patch", layeragent_protocol::new_id()));
        let applied = async {
            workspace_state(repo, scratch, &state).await?;
            apply_into(dest, &state, false).await
        }
        .await;
        let _ = std::fs::remove_file(&state);
        applied?;
        commit_base(dest, "team workspace base").await
    }
    .await;
    if result.is_err() {
        super::worktree_remove(repo, dest).await;
    }
    result
}

/// The member's changes relative to `base` written to `out`, and the paths
/// they touch (renames count as a deletion and an addition).
pub async fn diff(worktree: &Path, base: &str, out: &Path) -> Result<Vec<String>> {
    ensure_linked(worktree)?;
    checked(worktree, &["add", "-A"]).await?;
    checked(
        worktree,
        &[
            "diff",
            "--cached",
            "--binary",
            "--no-renames",
            &output_arg(out),
            base,
        ],
    )
    .await?;
    let names = out.with_extension("names");
    let listed = checked(
        worktree,
        &[
            "diff",
            "--cached",
            "--name-only",
            "--no-renames",
            "-z",
            &output_arg(&names),
            base,
        ],
    )
    .await;
    let files = read_names(&names);
    let _ = std::fs::remove_file(&names);
    listed?;
    files
}

/// The member's changes to `paths` only, relative to `base`, written to `out`.
pub async fn diff_paths(worktree: &Path, base: &str, paths: &[String], out: &Path) -> Result<()> {
    ensure_linked(worktree)?;
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_PATCH_FILES,
        "provide 1..{MAX_PATCH_FILES} paths"
    );
    checked(worktree, &["add", "-A"]).await?;
    let output = output_arg(out);
    let mut args = vec![
        "diff",
        "--cached",
        "--binary",
        "--no-renames",
        &output,
        base,
        "--",
    ];
    args.extend(paths.iter().map(String::as_str));
    checked(worktree, &args).await?;
    Ok(())
}

/// Whether `patch` applies cleanly to the working tree of `repo`; the operator's
/// index is not used, and nothing is written.
pub async fn check(repo: &Path, patch: &Path) -> Result<std::result::Result<(), String>> {
    ensure_repository(repo)?;
    let file = git_arg(patch);
    let (code, output) = run(
        repo,
        &["apply", "--check", "--binary", "--whitespace=nowarn", &file],
    )
    .await?;
    Ok(if code == 0 { Ok(()) } else { Err(output) })
}

/// Applies a checked patch to the working tree of `repo`. Git applies a patch
/// atomically: on failure nothing is changed.
pub async fn integrate(repo: &Path, patch: &Path) -> Result<()> {
    ensure_repository(repo)?;
    apply_into(repo, patch, false).await.map(|_| ())
}

/// Moves the worktree onto the team workspace as it is now and re-applies the
/// member's own changes with a three-way merge. Returns the new base and any
/// files left with conflict markers for the member to resolve.
pub async fn sync(
    repo: &Path,
    worktree: &Path,
    base: &str,
    scratch: &Path,
) -> Result<(String, Vec<String>)> {
    ensure_repository(repo)?;
    ensure_linked(worktree)?;
    std::fs::create_dir_all(scratch)?;
    let own = scratch.join(format!("own-{}.patch", layeragent_protocol::new_id()));
    let state = scratch.join(format!("state-{}.patch", layeragent_protocol::new_id()));
    let result = async {
        diff(worktree, base, &own).await?;
        workspace_state(repo, scratch, &state).await?;
        let target = head(repo).await?;
        checked(worktree, &["reset", "-q", "--hard", &target]).await?;
        checked(worktree, &["clean", "-fdq"]).await?;
        apply_into(worktree, &state, false).await?;
        let new_base = commit_base(worktree, "team workspace base").await?;
        let conflicts = apply_into(worktree, &own, true).await?;
        // Keep the index at the base so the next diff sees every change,
        // conflicted or not.
        checked(worktree, &["reset", "-q"]).await?;
        Ok((new_base, conflicts))
    }
    .await;
    let _ = std::fs::remove_file(&own);
    let _ = std::fs::remove_file(&state);
    result
}

/// Applies a patch handed over by another member on top of this worktree with a
/// three-way merge. Returns files left with conflict markers.
pub async fn receive(worktree: &Path, patch: &Path) -> Result<Vec<String>> {
    ensure_linked(worktree)?;
    let conflicts = apply_into(worktree, patch, true).await?;
    checked(worktree, &["reset", "-q"]).await?;
    Ok(conflicts)
}

/// Puts `paths` back to how they are at `base`, removing files added since.
/// Used after a handoff, so the sender no longer carries the transferred work.
pub async fn revert(worktree: &Path, base: &str, paths: &[String]) -> Result<()> {
    ensure_linked(worktree)?;
    if paths.is_empty() {
        return Ok(());
    }
    checked(worktree, &["add", "-A"]).await?;
    for chunk in paths.chunks(64) {
        let source = format!("--source={base}");
        let mut args = vec!["restore", &source, "--staged", "--worktree", "--"];
        args.extend(chunk.iter().map(String::as_str));
        checked(worktree, &args).await?;
    }
    checked(worktree, &["reset", "-q"]).await?;
    Ok(())
}

/// Removes a team worktree. Missing worktrees are not an error.
pub async fn remove(repo: &Path, worktree: &Path) {
    super::worktree_remove(repo, worktree).await;
}

/// Lines a patch adds that still carry merge conflict markers.
pub fn conflict_markers(patch: &[u8]) -> bool {
    String::from_utf8_lossy(patch).lines().any(|line| {
        ["+<<<<<<< ", "+>>>>>>> "]
            .iter()
            .any(|marker| line.starts_with(marker))
            || line == "+======="
    })
}

/// Whether a patch creates a symbolic link or turns a file into one. Approved
/// patches are applied outside the sandbox, so links are never carried.
pub fn adds_symlink(patch: &[u8]) -> bool {
    String::from_utf8_lossy(patch)
        .lines()
        .any(|line| line == "new file mode 120000" || line == "new mode 120000")
}

/// Rejects a worktree location outside the runtime's own team area.
pub fn ensure_under(root: &Path, worktree: &Path) -> Result<PathBuf> {
    let root = root.canonicalize()?;
    let worktree = worktree.canonicalize()?;
    if !worktree.starts_with(&root) {
        bail!("team worktree is outside the runtime team area");
    }
    Ok(worktree)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sh(dir: &Path, args: &[&str]) {
        let status = tokio::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .await
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    struct Repo {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        area: PathBuf,
    }

    async fn repo() -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        sh(&repo, &["init", "-q"]).await;
        std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(repo.join("b.txt"), "b\n").unwrap();
        sh(&repo, &["add", "-A"]).await;
        sh(&repo, &["commit", "-q", "-m", "base"]).await;
        let area = dir.path().join("teams");
        Repo {
            _dir: dir,
            repo,
            area,
        }
    }

    #[test]
    fn patch_paths_stay_inside_the_tree_and_away_from_secrets() {
        assert!(check_patch_path("src/lib.rs").is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "./a",
            ".git/config",
            "sub/.GIT/hooks",
            ".env",
            "a\\b",
        ] {
            assert!(check_patch_path(bad).is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symbolic_links_in_a_patch_are_detected() {
        let r = repo().await;
        let wt = r.area.join("m");
        let base = create(&r.repo, &wt, &r.area.join("tmp")).await.unwrap();
        std::os::unix::fs::symlink("/etc", wt.join("outside")).unwrap();
        let out = r.area.join("link.patch");
        diff(&wt, &base, &out).await.unwrap();
        assert!(adds_symlink(&std::fs::read(&out).unwrap()));
        assert!(!adds_symlink(b"+new file mode 120000\n"));
    }

    #[test]
    fn leftover_conflict_markers_are_detected_only_in_added_lines() {
        assert!(conflict_markers(b"+<<<<<<< ours\n+x\n+=======\n"));
        assert!(!conflict_markers(b"-<<<<<<< old\n context\n+fine\n"));
    }

    #[tokio::test]
    async fn a_worktree_starts_from_the_dirty_workspace_and_its_patch_is_only_the_members_work() {
        let r = repo().await;
        // Uncommitted and untracked operator work, plus a secret that must stay out.
        std::fs::write(r.repo.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(r.repo.join("new.txt"), "untracked\n").unwrap();
        std::fs::write(r.repo.join(".env"), "TOKEN=x\n").unwrap();
        sh(&r.repo, &["add", "b.txt"]).await;
        let index_before = std::fs::read(r.repo.join(".git/index")).unwrap();

        let wt = r.area.join("member");
        let base = create(&r.repo, &wt, &r.area.join("tmp")).await.unwrap();
        assert_eq!(base.len(), 40);
        assert_eq!(read(&wt.join("a.txt")), "one\nTWO\nthree\n");
        assert_eq!(read(&wt.join("new.txt")), "untracked\n");
        assert!(!wt.join(".env").exists(), "secrets are not copied");
        assert_eq!(
            std::fs::read(r.repo.join(".git/index")).unwrap(),
            index_before,
            "the operator's index is untouched"
        );

        std::fs::write(wt.join("b.txt"), "b changed\n").unwrap();
        std::fs::write(wt.join("added.txt"), "added\n").unwrap();
        let out = r.area.join("p.patch");
        let mut files = diff(&wt, &base, &out).await.unwrap();
        files.sort();
        assert_eq!(files, ["added.txt", "b.txt"]);
        let patch = read(&out);
        assert!(
            !patch.contains("TWO"),
            "operator changes are not the member's"
        );

        assert!(check(&r.repo, &out).await.unwrap().is_ok());
        integrate(&r.repo, &out).await.unwrap();
        assert_eq!(read(&r.repo.join("b.txt")), "b changed\n");
        assert_eq!(read(&r.repo.join("added.txt")), "added\n");
        assert_eq!(
            read(&r.repo.join("a.txt")),
            "one\nTWO\nthree\n",
            "operator work preserved"
        );
        remove(&r.repo, &wt).await;
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn sync_moves_onto_integrated_work_and_reports_real_conflicts() {
        let r = repo().await;
        let scratch = r.area.join("tmp");
        let first = r.area.join("first");
        let second = r.area.join("second");
        let base1 = create(&r.repo, &first, &scratch).await.unwrap();
        let base2 = create(&r.repo, &second, &scratch).await.unwrap();

        // The first member's change is integrated into the team workspace.
        std::fs::write(first.join("a.txt"), "ONE\ntwo\nthree\n").unwrap();
        let out = scratch.join("first.patch");
        diff(&first, &base1, &out).await.unwrap();
        integrate(&r.repo, &out).await.unwrap();

        // The second member edited another line: sync is clean and keeps both.
        std::fs::write(second.join("a.txt"), "one\ntwo\nTHREE\n").unwrap();
        let (base2b, conflicts) = sync(&r.repo, &second, &base2, &scratch).await.unwrap();
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(read(&second.join("a.txt")), "ONE\ntwo\nTHREE\n");
        let mine = scratch.join("second.patch");
        assert_eq!(diff(&second, &base2b, &mine).await.unwrap(), ["a.txt"]);
        assert!(!read(&mine).contains("+ONE"), "integrated work is now base");

        // Syncing the author after its own integration leaves nothing to submit.
        let (base1b, conflicts) = sync(&r.repo, &first, &base1, &scratch).await.unwrap();
        assert!(conflicts.is_empty());
        assert!(diff(&first, &base1b, &out).await.unwrap().is_empty());

        // Overlapping edits leave markers for the member instead of failing.
        integrate(&r.repo, &mine).await.unwrap();
        std::fs::write(first.join("a.txt"), "ONE\ntwo\nthird\n").unwrap();
        let (_, conflicts) = sync(&r.repo, &first, &base1b, &scratch).await.unwrap();
        assert_eq!(conflicts, ["a.txt"]);
        assert!(read(&first.join("a.txt")).contains("<<<<<<<"));
    }

    #[tokio::test]
    async fn a_stale_patch_is_refused_whole_and_the_workspace_is_untouched() {
        let r = repo().await;
        let scratch = r.area.join("tmp");
        let wt = r.area.join("m");
        let base = create(&r.repo, &wt, &scratch).await.unwrap();
        std::fs::write(wt.join("a.txt"), "one\nchanged\nthree\n").unwrap();
        std::fs::write(wt.join("b.txt"), "b2\n").unwrap();
        let out = scratch.join("m.patch");
        diff(&wt, &base, &out).await.unwrap();
        // The operator changed the same line meanwhile.
        std::fs::write(r.repo.join("a.txt"), "one\noperator\nthree\n").unwrap();
        assert!(check(&r.repo, &out).await.unwrap().is_err());
        assert!(integrate(&r.repo, &out).await.is_err());
        assert_eq!(read(&r.repo.join("b.txt")), "b\n", "no partial apply");
        assert_eq!(read(&r.repo.join("a.txt")), "one\noperator\nthree\n");
    }

    #[tokio::test]
    async fn handed_over_work_moves_between_worktrees() {
        let r = repo().await;
        let scratch = r.area.join("tmp");
        let from = r.area.join("from");
        let to = r.area.join("to");
        let base = create(&r.repo, &from, &scratch).await.unwrap();
        create(&r.repo, &to, &scratch).await.unwrap();
        std::fs::write(from.join("b.txt"), "half done\n").unwrap();
        std::fs::write(from.join("mine.txt"), "other work\n").unwrap();
        let out = scratch.join("carry.patch");
        diff(&from, &base, &out).await.unwrap();
        // Only the transferred path travels; the sender keeps its other work.
        let filtered = scratch.join("filtered.patch");
        diff_paths(&from, &base, &["b.txt".into()], &filtered)
            .await
            .unwrap();
        assert!(receive(&to, &filtered).await.unwrap().is_empty());
        assert_eq!(read(&to.join("b.txt")), "half done\n");
        revert(&from, &base, &["b.txt".into()]).await.unwrap();
        assert_eq!(read(&from.join("b.txt")), "b\n");
        assert_eq!(read(&from.join("mine.txt")), "other work\n");
        revert(&from, &base, &["mine.txt".into()]).await.unwrap();
        assert!(!from.join("mine.txt").exists(), "added files are removed");
    }

    #[tokio::test]
    async fn destructive_steps_refuse_anything_but_a_linked_worktree() {
        let r = repo().await;
        let scratch = r.area.join("tmp");
        std::fs::write(r.repo.join("a.txt"), "operator edit\n").unwrap();
        assert!(sync(&r.repo, &r.repo, "HEAD", &scratch).await.is_err());
        assert!(revert(&r.repo, "HEAD", &["a.txt".into()]).await.is_err());
        assert_eq!(read(&r.repo.join("a.txt")), "operator edit\n");
    }
}
