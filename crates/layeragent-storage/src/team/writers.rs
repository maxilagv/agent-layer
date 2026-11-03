//! Cooperative writers: per-member read/write access, path claims and patches.
//!
//! Every member reads the shared team workspace. A member switches to writing
//! for a work item it owns by claiming the paths it will change; the runtime
//! then runs its next activation in its own worktree. What it produces is a
//! patch, recorded with a runtime receipt and integrated into the team
//! workspace only after another member approves it. All checks repeat inside
//! the writer transaction.
use super::*;
use layeragent_protocol::TeamAccess;
use sha2::{Digest, Sha256};

/// Largest patch a member can submit or carry.
pub const MAX_PATCH_BYTES: usize = 2 * 1024 * 1024;
/// Most paths a member can reserve in one request.
const MAX_CLAIMS_PER_REQUEST: usize = 64;
const MAX_PATCH_FILES: usize = 256;

/// A member's writing state as the runtime needs it.
#[derive(Clone, Debug)]
pub struct WriterState {
    /// What the member asked for.
    pub access: TeamAccess,
    /// What its current or last activation runs with.
    pub applied: TeamAccess,
    pub worktree: Option<String>,
    pub base: Option<String>,
    /// The team workspace changed since the worktree base was taken.
    pub stale: bool,
    /// Reserved (path, work) pairs.
    pub claims: Vec<(String, String)>,
}

/// A patch computed by the runtime from the member's worktree.
#[derive(Clone, Debug)]
pub struct PatchSubmission {
    pub work_id: String,
    pub summary: String,
    pub base: String,
    pub files: Vec<String>,
    pub body: Vec<u8>,
    pub reviewer: Option<String>,
}

/// Unsubmitted work that moves with a handoff or is kept when a team ends.
#[derive(Clone, Debug)]
pub struct PatchCarry {
    pub base: String,
    pub files: Vec<String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// Approved and applied to the team workspace.
    Integrated,
    Rejected,
    /// Approved, but it no longer applies to the team workspace.
    Conflicted,
}
impl ReviewOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Integrated => "integrated",
            Self::Rejected => "rejected",
            Self::Conflicted => "conflicted",
        }
    }
}

/// A claim is a repository-relative file or directory prefix.
fn normalize_claim(path: &str) -> Result<String> {
    let mut path = path.trim();
    while let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let path = path.trim_end_matches('/');
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.starts_with('/')
            && !path.contains('\\')
            && !path.contains('\0'),
        "claim {path:?} must be a repository-relative path"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "claim {path:?} must be a plain relative path"
        );
        ensure!(
            !part.eq_ignore_ascii_case(".git"),
            "claim {path:?} touches Git metadata"
        );
    }
    Ok(path.to_string())
}

fn covers(claim: &str, path: &str) -> bool {
    path == claim
        || path
            .strip_prefix(claim)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn overlaps(a: &str, b: &str) -> bool {
    covers(a, b) || covers(b, a)
}

fn claims_of(c: &Connection, team: &str, member: &str) -> Result<Vec<(String, String)>> {
    let mut q = c.prepare(
        "SELECT path,work_id FROM team_path_claims WHERE team_id=?1 AND member_id=?2 ORDER BY path",
    )?;
    let rows = q
        .query_map(params![team, member], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn claim_list(claims: &[(String, String)]) -> Value {
    json!(
        claims
            .iter()
            .map(|(path, work)| json!({"path":path,"work_id":work}))
            .collect::<Vec<_>>()
    )
}

/// Validates a request to write `paths` for `work`; returns the paths the
/// member does not already hold.
fn write_checks(
    c: &Connection,
    team: &str,
    member: &str,
    work: &str,
    paths: &[String],
) -> Result<Vec<String>> {
    member_team(c, team, member)?;
    ensure!(
        team_state(c, team)? == "active",
        "team is not accepting new write access"
    );
    ensure!(
        options(c, team)?.scope.allow_write,
        "this team is read-only: the operator did not allow writing when the task was created"
    );
    let member_state: String = c.query_row(
        "SELECT state FROM team_members WHERE id=?1",
        [member],
        |r| r.get(0),
    )?;
    ensure!(
        !matches!(member_state.as_str(), "terminated" | "failed"),
        "member can no longer write"
    );
    let owned: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM team_work WHERE id=?1 AND team_id=?2 AND assignee=?3 AND state='in_progress')",
        params![work, team, member],
        |r| r.get(0),
    )?;
    ensure!(
        owned,
        "write access is granted for work you are doing: claim the work item with team_work_claim first"
    );
    ensure!(
        paths.len() <= MAX_CLAIMS_PER_REQUEST,
        "reserve at most {MAX_CLAIMS_PER_REQUEST} paths at a time"
    );
    let mut wanted = Vec::new();
    for path in paths {
        let path = normalize_claim(path)?;
        if !wanted.contains(&path) {
            wanted.push(path);
        }
    }
    let held = claims_of(c, team, member)?;
    ensure!(
        !wanted.is_empty() || held.iter().any(|(_, w)| w == work),
        "name the files or directories you will change for this work"
    );
    let others = {
        let mut q = c.prepare("SELECT p.path,p.work_id,m.name FROM team_path_claims p JOIN team_members m ON m.id=p.member_id WHERE p.team_id=?1 AND p.member_id<>?2")?;
        q.query_map(params![team, member], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut fresh = Vec::new();
    for path in wanted {
        if let Some((other, other_work, name)) = others.iter().find(|(p, _, _)| overlaps(p, &path))
        {
            anyhow::bail!(
                "{path} overlaps {other}, reserved by {name} for work {other_work}; coordinate with them or choose other paths"
            );
        }
        match held.iter().find(|(p, _)| p == &path) {
            Some((_, w)) if w == work => {}
            Some((_, w)) => anyhow::bail!("you already reserved {path} for work {w}"),
            None => fresh.push(path),
        }
    }
    Ok(fresh)
}

fn patch_meta(c: &Connection, team: &str, patch: &str) -> Result<Value> {
    c.query_row(
        "SELECT id,kind,work_id,author_member_id,recipient_member_id,state,summary,base,files,sha256,bytes,evidence_id,review_message_id,reviewer_member_id,reason,created_at,updated_at FROM team_patches WHERE id=?1 AND team_id=?2",
        params![patch, team],
        |r| {
            Ok(json!({
                "id":r.get::<_,String>(0)?,"kind":r.get::<_,String>(1)?,"work_id":r.get::<_,Option<String>>(2)?,
                "author_member_id":r.get::<_,String>(3)?,"recipient_member_id":r.get::<_,Option<String>>(4)?,
                "state":r.get::<_,String>(5)?,"summary":r.get::<_,String>(6)?,"base":r.get::<_,String>(7)?,
                "files":serde_json::from_str::<Value>(&r.get::<_,String>(8)?).unwrap_or(json!([])),
                "sha256":r.get::<_,String>(9)?,"bytes":r.get::<_,i64>(10)?,"evidence_id":r.get::<_,Option<String>>(11)?,
                "review_message_id":r.get::<_,Option<String>>(12)?,"reviewer_member_id":r.get::<_,Option<String>>(13)?,
                "reason":r.get::<_,Option<String>>(14)?,"created_at":r.get::<_,i64>(15)?,"updated_at":r.get::<_,i64>(16)?,
            }))
        },
    )
    .optional()?
    .context("patch is outside this team")
}

fn patch_body(c: &Connection, patch: &str) -> Result<Vec<u8>> {
    Ok(c.query_row(
        "SELECT body FROM team_patch_bodies WHERE patch_id=?1",
        [patch],
        |r| r.get(0),
    )?)
}

#[allow(clippy::too_many_arguments)]
fn insert_patch(
    c: &Connection,
    team: &str,
    kind: &str,
    state: &str,
    work: Option<&str>,
    author: &str,
    recipient: Option<&str>,
    summary: &str,
    base: &str,
    files: &[String],
    body: &[u8],
    evidence: Option<&str>,
    at: i64,
) -> Result<(String, String)> {
    ensure!(
        body.len() <= MAX_PATCH_BYTES,
        "patch is larger than {MAX_PATCH_BYTES} bytes; split the work into smaller items"
    );
    ensure!(
        files.len() <= MAX_PATCH_FILES,
        "patch touches more than {MAX_PATCH_FILES} files; split the work into smaller items"
    );
    let id = new_id();
    let sha = hex::encode(Sha256::digest(body));
    c.execute(
        "INSERT INTO team_patches(id,team_id,kind,work_id,author_member_id,recipient_member_id,state,summary,base,files,sha256,bytes,evidence_id,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)",
        params![id, team, kind, work, author, recipient, state, summary, base, serde_json::to_string(files)?, sha, body.len() as i64, evidence, at],
    )?;
    c.execute(
        "INSERT INTO team_patch_bodies(patch_id,body) VALUES(?1,?2)",
        params![id, body],
    )?;
    Ok((id, sha))
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// A peer message sent by the runtime on a member's behalf, with the same
/// validation and bounds as one the member writes.
#[allow(clippy::too_many_arguments)]
fn member_notice(
    c: &Connection,
    team: &str,
    sender: &str,
    recipient: &str,
    purpose: MessagePurpose,
    body: String,
    work: Option<String>,
    evidence: Option<String>,
    reply_to: Option<String>,
    key: String,
) -> Result<Value> {
    let limit = options(c, team)?.limits.max_message_bytes;
    let revision: i64 = c.query_row("SELECT revision FROM teams WHERE id=?1", [team], |r| {
        r.get(0)
    })?;
    send_message(
        c,
        team,
        &format!("member:{sender}"),
        Some(sender),
        "member",
        TeamMessageInput {
            idempotency_key: key,
            expected_revision: revision,
            recipient: Some(recipient.to_string()),
            purpose,
            body: clip(&body, limit.saturating_sub(8)),
            work_id: work,
            finding_id: None,
            evidence_id: evidence,
            reply_to,
            provenance: vec![],
            urgent: false,
        },
    )
}

impl Storage {
    pub async fn team_writer_state(&self, team: &str, member: &str) -> Result<WriterState> {
        let (team, member) = (team.to_string(), member.to_string());
        self.read(move |c| {
            let (access, applied, worktree, base, stale): (String, String, Option<String>, Option<String>, bool) = c.query_row(
                "SELECT access,access_applied,worktree,worktree_base,worktree_stale FROM team_members WHERE id=?1 AND team_id=?2",
                params![member, team],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            ).context("member is outside this team")?;
            Ok(WriterState {
                access: access.parse()?,
                applied: applied.parse()?,
                worktree,
                base,
                stale,
                claims: claims_of(c, &team, &member)?,
            })
        })
        .await
    }

    /// Read-only validation before the runtime creates a worktree, so a refused
    /// request costs nothing. The write itself checks everything again.
    pub async fn team_write_precheck(
        &self,
        team: &str,
        member: &str,
        work: &str,
        paths: &[String],
    ) -> Result<()> {
        let (team, member, work, paths) = (
            team.to_string(),
            member.to_string(),
            work.to_string(),
            paths.to_vec(),
        );
        self.read(move |c| write_checks(c, &team, &member, &work, &paths).map(|_| ()))
            .await
    }

    /// Switches a member to writing for `work`, reserving `paths`. A member
    /// without a worktree must be given the one the runtime created for it.
    pub async fn team_request_write(
        &self,
        team: &str,
        member: &str,
        work: &str,
        paths: &[String],
        worktree: Option<(String, String)>,
    ) -> Result<Value> {
        let (team, member, work, paths) = (
            team.to_string(),
            member.to_string(),
            work.to_string(),
            paths.to_vec(),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let fresh = write_checks(&tx, &team, &member, &work, &paths)?;
            let (current, applied): (Option<String>, String) = tx.query_row(
                "SELECT worktree,access_applied FROM team_members WHERE id=?1",
                [&member],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            ensure!(
                current.is_some() || worktree.is_some(),
                "no worktree prepared for this member"
            );
            let at = now_ms();
            for path in &fresh {
                tx.execute(
                    "INSERT INTO team_path_claims(team_id,path,member_id,work_id,created_at) VALUES(?1,?2,?3,?4,?5)",
                    params![team, path, member, work, at],
                )?;
            }
            let (path, base) = worktree.unzip();
            tx.execute(
                "UPDATE team_members SET access='write',worktree=COALESCE(worktree,?2),worktree_base=COALESCE(worktree_base,?3),updated_at=?4 WHERE id=?1",
                params![member, path, base, at],
            )?;
            let claims = claims_of(&tx, &team, &member)?;
            let applies = if applied == "write" { "now" } else { "next_activation" };
            bump(&tx, &team, at)?;
            event(&tx, &team, "team_access_changed", json!({"member_id":member,"access":"write","work_id":work,"claimed":fresh,"applies":applies}), at)?;
            tx.commit()?;
            Ok(json!({"access":"write","applies":applies,"claimed_paths":claim_list(&claims)}))
        })
        .await
    }

    /// Switches a member back to reading. Paths reserved for a patch that waits
    /// for review stay reserved until the review settles it.
    pub async fn team_release_write(&self, team: &str, member: &str) -> Result<Value> {
        let (team, member) = (team.to_string(), member.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            member_team(&tx, &team, &member)?;
            let applied: String = tx.query_row(
                "SELECT access_applied FROM team_members WHERE id=?1",
                [&member],
                |r| r.get(0),
            )?;
            let at = now_ms();
            tx.execute(
                "DELETE FROM team_path_claims WHERE team_id=?1 AND member_id=?2 AND work_id NOT IN (SELECT work_id FROM team_patches WHERE team_id=?1 AND author_member_id=?2 AND kind='submission' AND state='submitted')",
                params![team, member],
            )?;
            tx.execute(
                "UPDATE team_members SET access='read',updated_at=?2 WHERE id=?1",
                params![member, at],
            )?;
            let kept = claims_of(&tx, &team, &member)?;
            let applies = if applied == "read" { "now" } else { "next_activation" };
            bump(&tx, &team, at)?;
            event(&tx, &team, "team_access_changed", json!({"member_id":member,"access":"read","applies":applies}), at)?;
            tx.commit()?;
            Ok(json!({"access":"read","applies":applies,"kept_paths":claim_list(&kept)}))
        })
        .await
    }

    /// Records that the member's worktree now starts from the current team
    /// workspace.
    pub async fn team_worktree_synced(&self, team: &str, member: &str, base: &str) -> Result<()> {
        let (team, member, base) = (team.to_string(), member.to_string(), base.to_string());
        self.write(move |c| {
            let changed = c.execute(
                "UPDATE team_members SET worktree_base=?3,worktree_stale=0,updated_at=?4 WHERE id=?1 AND team_id=?2 AND worktree IS NOT NULL",
                params![member, team, base, now_ms()],
            )?;
            ensure!(changed == 1, "member has no worktree");
            Ok(())
        })
        .await
    }

    /// Records a member's patch for review, with a runtime receipt that a claim
    /// message can cite. Earlier open patches for the same work are superseded.
    pub async fn team_submit_patch(
        &self,
        team: &str,
        member: &str,
        submission: PatchSubmission,
    ) -> Result<Value> {
        let (team, member) = (team.to_string(), member.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            member_team(&tx, &team, &member)?;
            ensure!(team_state(&tx, &team)? == "active", "team is not accepting patches");
            let PatchSubmission { work_id, summary, base, files, body, reviewer } = submission;
            let (applied, current_base, stale): (String, Option<String>, bool) = tx.query_row(
                "SELECT access_applied,worktree_base,worktree_stale FROM team_members WHERE id=?1",
                [&member],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            ensure!(applied == "write", "this activation is read-only; ask for write access with team_access and submit from your next activation");
            ensure!(current_base.as_deref() == Some(base.as_str()) && !stale, "your worktree is behind the team workspace; it must be synced before submitting");
            let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM team_work WHERE id=?1 AND team_id=?2 AND assignee=?3 AND state='in_progress')", params![work_id, team, member], |r| r.get(0))?;
            ensure!(owned, "a patch is submitted for in-progress work you own");
            let summary = layeragent_protocol::redact::redact_text(summary.trim());
            ensure!(!summary.is_empty() && summary.len() <= 2000, "patch summary must contain 1..2000 bytes");
            ensure!(!files.is_empty(), "your worktree has no changes to submit");
            let claims: Vec<String> = claims_of(&tx, &team, &member)?.into_iter().filter(|(_, w)| w == &work_id).map(|(p, _)| p).collect();
            let uncovered: Vec<&String> = files.iter().filter(|file| !claims.iter().any(|claim| covers(claim, file))).collect();
            ensure!(
                uncovered.is_empty(),
                "the patch changes paths you did not reserve for this work: {}; reserve them with team_access (mode write) or revert those changes",
                uncovered.iter().take(10).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
            if let Some(reviewer) = &reviewer {
                member_team(&tx, &team, reviewer)?;
                ensure!(reviewer != &member, "another member must review your patch");
            }
            let at = now_ms();
            tx.execute(
                "UPDATE team_patches SET state='superseded',reason='replaced by a newer submission',updated_at=?4 WHERE team_id=?1 AND work_id=?2 AND author_member_id=?3 AND kind='submission' AND state IN ('submitted','rejected','conflicted')",
                params![team, work_id, member, at],
            )?;
            let sha = hex::encode(Sha256::digest(&body));
            let evidence = insert_evidence(&tx, &team, &member, EvidenceRecord {
                finding: None,
                snapshot: sha.clone(),
                files: json!(files),
                exclusions: json!([]),
                configuration: json!({"work_id":work_id,"base":base}),
                tool: "patch".into(),
                tool_version: Some(env!("CARGO_PKG_VERSION").into()),
                invocation: format!("git diff --binary {base} (member worktree)"),
                exit_code: Some(0),
                expected: None,
                observed: Some(json!({"sha256":sha,"bytes":body.len(),"files":files.len()}).to_string()),
                error: None,
                limitations: json!(["a patch receipt records what changed, not that it is correct; review and verification are separate"]),
                negative_control: None,
                artifacts: json!([]),
                expectation_met: true,
            })?;
            let evidence_id = evidence["id"].as_str().context("evidence id")?.to_string();
            let (id, _) = insert_patch(&tx, &team, "submission", "submitted", Some(&work_id), &member, None, &summary, &base, &files, &body, Some(&evidence_id), at)?;
            tx.execute("UPDATE team_evidence SET artifacts=?2 WHERE id=?1", params![evidence_id, json!([{"patch_id":id}]).to_string()])?;
            event(&tx, &team, "team_patch_submitted", json!({"patch_id":id,"work_id":work_id,"author":member,"files":files,"bytes":body.len()}), at)?;
            let mut request = Value::Null;
            if let Some(reviewer) = &reviewer {
                let sent = member_notice(
                    &tx, &team, &member, reviewer, MessagePurpose::VerificationRequest,
                    format!("Please review patch {id} for work {work_id}: {summary}\nRead it with team_patch_show and decide with team_patch_review."),
                    Some(work_id.clone()), Some(evidence_id.clone()), None, format!("patch-review-{id}"),
                )?;
                if sent["state"] == "accepted" {
                    tx.execute("UPDATE team_patches SET review_message_id=?2 WHERE id=?1", params![id, sent["id"].as_str()])?;
                }
                request = sent;
            }
            tx.commit()?;
            Ok(json!({"patch_id":id,"state":"submitted","evidence_id":evidence_id,"files":files,"bytes":body.len(),"review_request":request}))
        })
        .await
    }

    pub async fn team_patch(&self, team: &str, patch: &str) -> Result<(Value, Vec<u8>)> {
        let (team, patch) = (team.to_string(), patch.to_string());
        self.read(move |c| {
            let meta = patch_meta(c, &team, &patch)?;
            Ok((meta, patch_body(c, &patch)?))
        })
        .await
    }

    /// Validates a review before the runtime applies anything.
    pub async fn team_review_precheck(
        &self,
        team: &str,
        reviewer: &str,
        patch: &str,
    ) -> Result<(Value, Vec<u8>)> {
        let (team, reviewer, patch) = (team.to_string(), reviewer.to_string(), patch.to_string());
        self.read(move |c| {
            member_team(c, &team, &reviewer)?;
            ensure!(
                team_state(c, &team)? == "active",
                "team is not accepting reviews"
            );
            let meta = patch_meta(c, &team, &patch)?;
            ensure!(
                meta["kind"] == "submission",
                "only submitted patches are reviewed"
            );
            ensure!(
                meta["state"] == "submitted",
                "patch is {}; only a submitted patch can be reviewed",
                meta["state"].as_str().unwrap_or("?")
            );
            ensure!(
                meta["author_member_id"] != reviewer.as_str(),
                "another member must review this patch"
            );
            Ok((meta, patch_body(c, &patch)?))
        })
        .await
    }

    /// Records a review. An integrated patch completes its work, releases its
    /// paths and marks every worktree as behind; the author is told either way.
    pub async fn team_review_record(
        &self,
        team: &str,
        reviewer: &str,
        patch: &str,
        outcome: ReviewOutcome,
        reason: &str,
    ) -> Result<Value> {
        let (team, reviewer, patch, reason) = (
            team.to_string(),
            reviewer.to_string(),
            patch.to_string(),
            layeragent_protocol::redact::redact_text(reason.trim()),
        );
        ensure!(!reason.is_empty(), "review reason required");
        self.write(move |c| {
            let tx = c.transaction()?;
            member_team(&tx, &team, &reviewer)?;
            let meta = patch_meta(&tx, &team, &patch)?;
            ensure!(meta["kind"] == "submission" && meta["state"] == "submitted", TeamConflict);
            let author = meta["author_member_id"].as_str().context("author")?.to_string();
            ensure!(author != reviewer, "another member must review this patch");
            let work = meta["work_id"].as_str().map(str::to_string);
            let at = now_ms();
            let state = outcome.as_str();
            tx.execute(
                "UPDATE team_patches SET state=?2,reviewer_member_id=?3,reason=?4,updated_at=?5 WHERE id=?1",
                params![patch, state, reviewer, reason, at],
            )?;
            let mut evidence = meta["evidence_id"].as_str().map(str::to_string);
            match outcome {
                ReviewOutcome::Integrated => {
                    let files: Vec<String> = serde_json::from_value(meta["files"].clone()).unwrap_or_default();
                    let receipt = insert_evidence(&tx, &team, &reviewer, EvidenceRecord {
                        finding: None,
                        snapshot: meta["sha256"].as_str().unwrap_or_default().to_string(),
                        files: json!(files),
                        exclusions: json!([]),
                        configuration: json!({"patch_id":patch,"work_id":work,"author":author}),
                        tool: "integration".into(),
                        tool_version: Some(env!("CARGO_PKG_VERSION").into()),
                        invocation: "git apply --binary (team workspace)".into(),
                        exit_code: Some(0),
                        expected: None,
                        observed: Some(json!({"applied":true,"files":files.len()}).to_string()),
                        error: None,
                        limitations: json!(["applied to the working tree only; nothing was committed"]),
                        negative_control: None,
                        artifacts: json!([{"patch_id":patch}]),
                        expectation_met: true,
                    })?;
                    evidence = receipt["id"].as_str().map(str::to_string);
                    if let Some(work) = &work {
                        tx.execute("UPDATE team_work SET state='completed',revision=revision+1,updated_at=?3 WHERE id=?1 AND team_id=?2 AND state='in_progress'", params![work, team, at])?;
                        tx.execute("UPDATE team_members SET current_work_id=NULL,work_state='completed',updated_at=?3 WHERE team_id=?1 AND current_work_id=?2", params![team, work, at])?;
                        tx.execute("DELETE FROM team_path_claims WHERE team_id=?1 AND work_id=?2", params![team, work])?;
                    }
                    // Every worktree now starts from an older workspace.
                    tx.execute("UPDATE team_members SET worktree_stale=1 WHERE team_id=?1 AND worktree IS NOT NULL", [&team])?;
                }
                ReviewOutcome::Conflicted => {
                    tx.execute("UPDATE team_members SET worktree_stale=1 WHERE id=?1 AND worktree IS NOT NULL", [&author])?;
                }
                ReviewOutcome::Rejected => {}
            }
            event(&tx, &team, &format!("team_patch_{state}"), json!({"patch_id":patch,"work_id":work,"author":author,"reviewer":reviewer,"reason":reason}), at)?;
            let deliverable: bool = tx.query_row("SELECT state NOT IN ('terminated','failed') FROM team_members WHERE id=?1", [&author], |r| r.get(0))?;
            let mut notice = Value::Null;
            if deliverable {
                let body = match outcome {
                    ReviewOutcome::Integrated => format!("Patch {patch} was approved and applied to the team workspace; its work is completed. Reason: {reason}"),
                    ReviewOutcome::Rejected => format!("Patch {patch} was rejected. Reason: {reason}\nYour work stays in progress: change it in your worktree and submit again."),
                    ReviewOutcome::Conflicted => format!("Patch {patch} was approved but no longer applies to the team workspace: {reason}\nSubmit again: your worktree is synced first, and conflicts are left marked for you to resolve."),
                };
                // Answer the review request when it went to this reviewer.
                let request = meta["review_message_id"].as_str().map(str::to_string);
                let answers: bool = match &request {
                    Some(id) => tx.query_row("SELECT EXISTS(SELECT 1 FROM team_deliveries WHERE message_id=?1 AND recipient_member_id=?2)", params![id, reviewer], |r| r.get(0))?,
                    None => false,
                };
                let purpose = if answers { MessagePurpose::Response } else { MessagePurpose::Observation };
                notice = member_notice(&tx, &team, &reviewer, &author, purpose, body, work.clone(), evidence.clone(), request.filter(|_| answers), format!("patch-{state}-{patch}"))?;
            }
            bump(&tx, &team, at)?;
            tx.commit()?;
            Ok(json!({"patch_id":patch,"state":state,"work_id":work,"evidence_id":evidence,"author_notified":notice["state"]=="accepted"}))
        })
        .await
    }

    /// A handoff message that also carries the sender's unsubmitted changes for
    /// the work, applied later in the recipient's worktree.
    pub async fn team_member_handoff(
        &self,
        team: &str,
        member: &str,
        input: TeamMessageInput,
        carry: Option<PatchCarry>,
    ) -> Result<Value> {
        let (team, member) = (team.to_string(), member.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            member_team(&tx, &team, &member)?;
            ensure!(input.purpose == MessagePurpose::Handoff, "not a handoff");
            let actor = format!("member:{member}");
            let (work, recipient) = (input.work_id.clone(), input.recipient.clone());
            let mut result = send_message(&tx, &team, &actor, Some(&member), "member", input)?;
            if result["state"] == "accepted"
                && result.get("carried_patch").is_none()
                && let Some(carry) = carry
                && !carry.files.is_empty()
            {
                let at = now_ms();
                let (id, _) = insert_patch(&tx, &team, "handoff", "pending", work.as_deref(), &member, recipient.as_deref(), "unsubmitted work handed over with the work item", &carry.base, &carry.files, &carry.body, None, at)?;
                event(&tx, &team, "team_patch_carried", json!({"patch_id":id,"work_id":work,"from":member,"to":recipient,"files":carry.files}), at)?;
                result["carried_patch"] = json!(id);
            }
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// Handed-over work waiting to be applied in this member's worktree.
    pub async fn team_pending_carries(
        &self,
        team: &str,
        member: &str,
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let (team, member) = (team.to_string(), member.to_string());
        self.read(move |c| {
            let mut q = c.prepare("SELECT p.id,b.body FROM team_patches p JOIN team_patch_bodies b ON b.patch_id=p.id WHERE p.team_id=?1 AND p.recipient_member_id=?2 AND p.kind='handoff' AND p.state='pending' ORDER BY p.created_at,p.rowid")?;
            let rows = q
                .query_map(params![team, member], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    pub async fn team_carry_received(
        &self,
        team: &str,
        member: &str,
        patch: &str,
        conflicts: &[String],
    ) -> Result<()> {
        let (team, member, patch, conflicts) = (
            team.to_string(),
            member.to_string(),
            patch.to_string(),
            conflicts.to_vec(),
        );
        self.write(move |c| {
            let tx = c.transaction()?;
            let at = now_ms();
            let reason = (!conflicts.is_empty()).then(|| format!("conflict markers left in: {}", conflicts.join(", ")));
            let changed = tx.execute("UPDATE team_patches SET state='received',reason=?4,updated_at=?5 WHERE id=?1 AND team_id=?2 AND recipient_member_id=?3 AND kind='handoff' AND state='pending'", params![patch, team, member, reason, at])?;
            ensure!(changed == 1, TeamConflict);
            event(&tx, &team, "team_patch_received", json!({"patch_id":patch,"member_id":member,"conflicts":conflicts}), at)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Members that own a worktree: (member, worktree, base).
    pub async fn team_worktrees(
        &self,
        team: &str,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let team = team.to_string();
        self.read(move |c| {
            let mut q = c.prepare("SELECT id,worktree,worktree_base FROM team_members WHERE team_id=?1 AND worktree IS NOT NULL ORDER BY created_at,rowid")?;
            let rows = q
                .query_map([&team], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
    }

    /// Forgets a member's worktree once the team has ended, keeping any
    /// unsubmitted changes as a retained patch so nothing is lost.
    pub async fn team_retire_worktree(
        &self,
        team: &str,
        member: &str,
        retained: Option<PatchCarry>,
    ) -> Result<Option<String>> {
        let (team, member) = (team.to_string(), member.to_string());
        self.write(move |c| {
            let tx = c.transaction()?;
            let at = now_ms();
            let mut kept = None;
            if let Some(carry) = retained.filter(|carry| !carry.files.is_empty()) {
                let work: Option<String> = tx.query_row("SELECT current_work_id FROM team_members WHERE id=?1", [&member], |r| r.get(0))?;
                let (id, _) = insert_patch(&tx, &team, "retained", "retained", work.as_deref(), &member, None, "unsubmitted work kept when the team ended", &carry.base, &carry.files, &carry.body, None, at)?;
                kept = Some(id);
            }
            tx.execute("DELETE FROM team_path_claims WHERE team_id=?1 AND member_id=?2", params![team, member])?;
            tx.execute("UPDATE team_members SET worktree=NULL,worktree_base=NULL,worktree_stale=0,access='read',updated_at=?3 WHERE id=?1 AND team_id=?2", params![member, team, at])?;
            event(&tx, &team, "team_worktree_retired", json!({"member_id":member,"retained_patch":kept}), at)?;
            tx.commit()?;
            Ok(kept)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_are_prefixes_on_path_boundaries() {
        assert!(covers("src", "src/lib.rs"));
        assert!(covers("src/lib.rs", "src/lib.rs"));
        assert!(!covers("src", "srcx/lib.rs"));
        assert!(overlaps("src/a", "src"));
        assert!(!overlaps("src/a", "src/b"));
        assert_eq!(normalize_claim("./src/").ok().as_deref(), Some("src"));
        for bad in ["", "/etc", "../x", "a/../b", ".git", "a/.git/x", "a\\b"] {
            assert!(normalize_claim(bad).is_err(), "{bad}");
        }
    }
}
