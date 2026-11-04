use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use layeragent_config::{Config, Policy, private_dir};
use layeragent_protocol::validate_name;
use layeragent_sandbox::{ExecRequest, Sandbox};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

mod sourcing;
pub use sourcing::{accept, import, quarantined, reject, review, search};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub hash: String,
    pub verified: bool,
    /// The operator read and accepted it (a skill without fixtures).
    #[serde(default)]
    pub reviewed: bool,
}
#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub argv: Vec<String>,
    pub stdin: Option<String>,
    pub expected_stdout: String,
}
#[derive(Serialize, Deserialize)]
struct Verification {
    hash: String,
    tests: usize,
}
pub fn discover(home: &Path, config: &Config) -> Result<Vec<Skill>> {
    let mut found = BTreeMap::new();
    for root in std::iter::once(home.join("skills")).chain(config.skills.iter().cloned()) {
        if !root.exists() {
            continue;
        }
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if !path.is_dir() || path.is_symlink() {
                continue;
            }
            let file = path.join("SKILL.md");
            if !file.is_file() || file.is_symlink() {
                continue;
            }
            let raw = std::fs::read_to_string(&file)?;
            ensure!(raw.len() <= 262_144, "skill is too large");
            let meta = parse_frontmatter(&raw)?;
            let hash = tree_hash(&path)?;
            let trusted = home
                .join("skills")
                .canonicalize()
                .is_ok_and(|root| path.canonicalize().is_ok_and(|path| path.starts_with(root)));
            let verified = trusted
                && std::fs::read_to_string(path.join(".verification.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str::<Verification>(&s).ok())
                    .is_some_and(|v| v.hash == hash && v.tests > 0);
            let reviewed = trusted
                && std::fs::read_to_string(path.join(sourcing::REVIEW))
                    .ok()
                    .and_then(|s| serde_json::from_str::<sourcing::Review>(&s).ok())
                    .is_some_and(|r| r.hash == hash);
            ensure!(
                !found.contains_key(&meta.name),
                "duplicate skill name {}",
                meta.name
            );
            found.insert(
                meta.name.clone(),
                Skill {
                    name: meta.name,
                    description: meta.description,
                    path,
                    hash,
                    verified,
                    reviewed,
                },
            );
        }
    }
    Ok(found.into_values().collect())
}
fn parse_frontmatter(raw: &str) -> Result<Frontmatter> {
    let body = raw
        .strip_prefix("---\n")
        .context("skill needs YAML frontmatter")?;
    let end = body.find("\n---").context("unterminated frontmatter")?;
    let m: Frontmatter = serde_saphyr::from_str(&body[..end])?;
    validate_name(&m.name)?;
    ensure!(!m.description.is_empty(), "skill description required");
    Ok(m)
}
pub fn tree_hash(root: &Path) -> Result<String> {
    let mut entries = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.path().to_path_buf());
    let mut hash = Sha256::new();
    for e in entries {
        ensure!(!e.path_is_symlink(), "skills cannot contain symlinks");
        // Records about the skill are not part of it.
        let record = [".verification.json", sourcing::REVIEW, sourcing::PROVENANCE]
            .contains(&e.file_name().to_string_lossy().as_ref());
        if e.file_type().is_file() && !record {
            let relative = e.path().strip_prefix(root)?;
            let bytes = std::fs::read(e.path())?;
            hash.update((relative.as_os_str().len() as u64).to_le_bytes());
            hash.update(relative.to_string_lossy().as_bytes());
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(&bytes);
        }
    }
    Ok(hex::encode(hash.finalize()))
}
pub fn draft(
    home: &Path,
    name: &str,
    body: &str,
    files: &BTreeMap<String, String>,
    tests: &[TestCase],
) -> Result<PathBuf> {
    let inputs = serde_json::json!({"body":body,"files":files,"tests":tests});
    let mut checked = inputs.clone();
    ensure!(
        layeragent_protocol::redact::redact_value(&mut checked) == 0,
        "skill draft contains sensitive credentials; remove them before staging"
    );
    let _lease = registry_lease(home)?;
    validate_name(name)?;
    let parsed = parse_frontmatter(body)?;
    ensure!(parsed.name == name, "skill name mismatch");
    // Without fixtures a draft is a guide: it cannot verify itself, so it
    // waits for the operator's review instead of staging for promotion.
    let staging = home.join(if tests.is_empty() {
        sourcing::QUARANTINE
    } else {
        "skill-staging"
    });
    private_dir(&staging)?;
    let dest = staging.join(name);
    ensure!(
        !dest.exists(),
        "draft already exists; remove or version it first"
    );
    std::fs::create_dir(&dest)?;
    let result = (|| {
        std::fs::write(dest.join("SKILL.md"), body)?;
        for (path, data) in files {
            let p = Path::new(path);
            ensure!(
                !p.is_absolute()
                    && p.components()
                        .all(|c| matches!(c, std::path::Component::Normal(_))),
                "invalid skill file path"
            );
            ensure!(
                path != "SKILL.md" && path != "tests.json" && !path.starts_with('.'),
                "reserved skill file"
            );
            let target = dest.join(p);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, data)?;
        }
        if tests.is_empty() {
            std::fs::write(
                dest.join(sourcing::PROVENANCE),
                serde_json::to_vec_pretty(
                    &json!({"source":"agent draft","imported_at":layeragent_protocol::now_ms(),"files":files.keys().collect::<Vec<_>>(),"scripts":[]}),
                )?,
            )?;
        } else {
            std::fs::write(dest.join("tests.json"), serde_json::to_vec_pretty(tests)?)?;
        }
        Ok(dest.clone())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    result
}
pub async fn verify_and_promote(
    home: &Path,
    name: &str,
    sandbox: &dyn Sandbox,
    cancel: CancellationToken,
) -> Result<Value> {
    let _lease = registry_lease(home)?;
    validate_name(name)?;
    let source = home.join("skill-staging").join(name);
    ensure!(
        source.is_dir() && !source.is_symlink()
            || !home.join(sourcing::QUARANTINE).join(name).is_dir(),
        "skill {name} has no fixtures, so it cannot verify itself; the operator accepts it with layeragent skills accept {name}"
    );
    ensure!(source.is_dir() && !source.is_symlink(), "draft not found");
    let before = tree_hash(&source)?;
    let cases: Vec<TestCase> = serde_json::from_slice(&std::fs::read(source.join("tests.json"))?)?;
    ensure!(
        !cases.is_empty() && cases.len() <= 32,
        "invalid fixture count"
    );
    for test in &cases {
        let policy = Policy {
            write: false,
            shell: false,
            network: false,
            timeout_secs: 30,
            ..Policy::default()
        };
        let out = sandbox
            .execute(
                ExecRequest {
                    argv: test.argv.clone(),
                    workspace: source.canonicalize()?,
                    policy,
                    stdin: test.stdin.clone(),
                },
                cancel.child_token(),
                None,
            )
            .await?;
        ensure!(
            out.code == Some(0)
                && !out.interrupted
                && !out.truncated
                && out.stdout == test.expected_stdout,
            "skill fixture failed"
        );
    }
    ensure!(
        tree_hash(&source)? == before,
        "skill changed during verification"
    );
    ensure!(
        !cancel.is_cancelled(),
        "skill verification cancelled before promotion"
    );
    std::fs::write(
        source.join(".verification.json"),
        serde_json::to_vec(&Verification {
            hash: before.clone(),
            tests: cases.len(),
        })?,
    )?;
    let registry = home.join("skills");
    private_dir(&registry)?;
    let active = registry.join(name);
    let mut previous = None;
    if active.exists() {
        let archive = home.join("skill-versions");
        private_dir(&archive)?;
        let archived = archive.join(format!("{name}-{}", tree_hash(&active)?));
        ensure!(
            !archived.exists(),
            "this previous version is already archived"
        );
        std::fs::rename(&active, &archived)?;
        previous = Some(archived);
    }
    if let Err(error) = std::fs::rename(&source, &active) {
        if let Some(previous) = previous {
            std::fs::rename(previous, &active)
                .context("restoring previous skill after failed promotion")?;
        }
        return Err(error.into());
    }
    Ok(json!({"skill":name,"hash":before,"tests_passed":cases.len(),"active":true}))
}
pub fn load(skill: &Skill) -> Result<String> {
    ensure!(
        tree_hash(&skill.path)? == skill.hash,
        "skill changed; rediscover before loading"
    );
    Ok(std::fs::read_to_string(skill.path.join("SKILL.md"))?)
}
pub fn rollback(home: &Path, name: &str, hash: &str) -> Result<()> {
    let _lease = registry_lease(home)?;
    validate_name(name)?;
    ensure!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid version hash"
    );
    let old = home.join("skill-versions").join(format!("{name}-{hash}"));
    ensure!(tree_hash(&old)? == hash, "archived skill hash mismatch");
    let current = home.join("skills").join(name);
    private_dir(&home.join("skill-staging"))?;
    private_dir(&home.join("skills"))?;
    let temp = home
        .join("skill-staging")
        .join(format!("{name}-rollback-{}", layeragent_protocol::new_id()));
    if current.exists() {
        std::fs::rename(&current, &temp)?;
    }
    if let Err(error) = std::fs::rename(old, &current) {
        if temp.exists() {
            std::fs::rename(&temp, current).context("restoring skill after failed rollback")?;
        }
        return Err(error.into());
    }
    Ok(())
}

fn registry_lease(home: &Path) -> Result<std::fs::File> {
    private_dir(home)?;
    let lease = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join(".skills.lock"))?;
    lease
        .try_lock_exclusive()
        .context("another skill registry operation is active")?;
    Ok(lease)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use layeragent_sandbox::ExecOutput;

    // These doubles test promotion policy; native confinement is tested separately.
    struct FixtureSandbox {
        pass: bool,
    }
    #[async_trait]
    impl Sandbox for FixtureSandbox {
        async fn execute(
            &self,
            request: ExecRequest,
            _: CancellationToken,
            _: Option<tokio::sync::mpsc::Sender<String>>,
        ) -> Result<ExecOutput> {
            assert!(!request.policy.write && !request.policy.network);
            Ok(ExecOutput {
                code: Some(0),
                stdout: if self.pass { "42\n" } else { "wrong\n" }.into(),
                stderr: String::new(),
                interrupted: false,
                truncated: false,
            })
        }
    }
    fn stage(home: &Path, description: &str) -> Result<PathBuf> {
        draft(
            home,
            "answer",
            &format!("---\nname: answer\ndescription: {description}\n---\nCalculate the answer."),
            &BTreeMap::new(),
            &[TestCase {
                argv: vec!["python3".into(), "-c".into(), "print(6 * 7)".into()],
                stdin: None,
                expected_stdout: "42\n".into(),
            }],
        )
    }
    #[tokio::test]
    async fn failed_fixture_stays_staged_and_verified_versions_can_roll_back() -> Result<()> {
        let home = tempfile::tempdir()?;
        stage(home.path(), "First version")?;
        assert!(
            verify_and_promote(
                home.path(),
                "answer",
                &FixtureSandbox { pass: false },
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(home.path().join("skill-staging/answer").is_dir());
        assert!(!home.path().join("skills/answer").exists());
        verify_and_promote(
            home.path(),
            "answer",
            &FixtureSandbox { pass: true },
            CancellationToken::new(),
        )
        .await?;
        let first = discover(home.path(), &Config::default())?.remove(0);
        assert!(first.verified);
        stage(home.path(), "Second version")?;
        verify_and_promote(
            home.path(),
            "answer",
            &FixtureSandbox { pass: true },
            CancellationToken::new(),
        )
        .await?;
        rollback(home.path(), "answer", &first.hash)?;
        assert_eq!(
            discover(home.path(), &Config::default())?[0].hash,
            first.hash
        );
        Ok(())
    }
    fn skill_dir(root: &Path, name: &str, extra: &str, body: &str) -> Result<PathBuf> {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Guide for {name}\n{extra}---\n{body}\n"),
        )?;
        Ok(dir)
    }

    #[test]
    fn imported_skills_wait_for_review_and_only_accepted_ones_are_offered() -> Result<()> {
        let home = tempfile::tempdir()?;
        let catalog = tempfile::tempdir()?;
        // A catalog in the common SKILL.md format, with extra frontmatter keys.
        let web = skill_dir(
            &catalog.path().join("skills"),
            "frontend-design",
            "license: Apache-2.0\n",
            "Design bold pages. See https://example.com/guide for more.",
        )?;
        std::fs::write(web.join("build.sh"), "echo build\n")?;
        let leaky = skill_dir(&catalog.path().join("skills"), "leaky", "", "Use the key.")?;
        std::fs::write(
            leaky.join("notes.txt"),
            "token sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ\n",
        )?;
        #[cfg(unix)]
        {
            let linked = skill_dir(
                &catalog.path().join("skills"),
                "linked",
                "",
                "Follow the link.",
            )?;
            std::os::unix::fs::symlink("/etc/hostname", linked.join("host"))?;
        }

        let imported = import(home.path(), catalog.path(), "test catalog")?;
        let status = |name: &str| {
            imported
                .iter()
                .find(|s| {
                    s["name"] == name || s["path"].as_str().is_some_and(|p| p.ends_with(name))
                })
                .map(|s| s["status"].as_str().unwrap_or("").to_string())
        };
        assert_eq!(
            status("frontend-design").as_deref(),
            Some("quarantined"),
            "{imported:?}"
        );
        assert_eq!(
            status("leaky").as_deref(),
            Some("refused"),
            "credentials: {imported:?}"
        );
        #[cfg(unix)]
        assert_eq!(
            status("linked").as_deref(),
            Some("refused"),
            "symlinks: {imported:?}"
        );

        assert!(
            discover(home.path(), &Config::default())?.is_empty(),
            "quarantine is not offered"
        );
        let waiting = quarantined(home.path())?;
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0]["scripts"], json!(["build.sh"]));
        let read = review(home.path(), "frontend-design")?;
        assert!(
            read["skill_md"]
                .as_str()
                .unwrap_or("")
                .contains("Design bold pages")
        );
        assert_eq!(read["links"], json!(["https://example.com/guide"]));
        assert!(
            read["source"]
                .as_str()
                .unwrap_or("")
                .starts_with("test catalog:")
        );
        let found = search(home.path(), &Config::default(), "frontend design", 5)?;
        assert_eq!(found[0]["status"], "quarantined");

        accept(home.path(), "frontend-design")?;
        let active = discover(home.path(), &Config::default())?;
        assert_eq!(active.len(), 1);
        assert!(active[0].reviewed && !active[0].verified);
        assert!(
            load(&active[0])?.contains("Design bold pages"),
            "the review record does not change its hash"
        );
        assert!(quarantined(home.path())?.is_empty());
        let found = search(home.path(), &Config::default(), "bold pages", 5)?;
        assert_eq!(found[0]["status"], "reviewed");
        assert!(
            found[0]["snippet"]
                .as_str()
                .unwrap_or("")
                .contains("bold pages")
        );
        // A changed skill is no longer the one the operator accepted.
        std::fs::write(
            active[0].path.join("SKILL.md"),
            "---\nname: frontend-design\ndescription: changed\n---\nOther.\n",
        )?;
        assert!(!discover(home.path(), &Config::default())?[0].reviewed);

        // A re-import replaces the waiting copy; a reject drops it.
        skill_dir(
            &catalog.path().join("skills"),
            "second",
            "",
            "Another guide.",
        )?;
        import(home.path(), catalog.path(), "again")?;
        assert!(
            quarantined(home.path())?
                .iter()
                .any(|s| s["name"] == "second")
        );
        reject(home.path(), "second")?;
        assert!(
            quarantined(home.path())?
                .iter()
                .all(|s| s["name"] != "second")
        );
        assert!(review(home.path(), "../skills").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn a_guide_drafted_without_fixtures_needs_the_operator() -> Result<()> {
        let home = tempfile::tempdir()?;
        draft(
            home.path(),
            "bike-brand",
            "---\nname: bike-brand\ndescription: Colors and tone of the bicycle store\n---\nUse deep green and warm grey.",
            &BTreeMap::new(),
            &[],
        )?;
        assert!(discover(home.path(), &Config::default())?.is_empty());
        let refused = verify_and_promote(
            home.path(),
            "bike-brand",
            &FixtureSandbox { pass: true },
            CancellationToken::new(),
        )
        .await
        .expect_err("no fixtures");
        assert!(
            refused
                .to_string()
                .contains("layeragent skills accept bike-brand"),
            "{refused}"
        );
        assert_eq!(quarantined(home.path())?[0]["source"], "agent draft");
        accept(home.path(), "bike-brand")?;
        assert!(discover(home.path(), &Config::default())?[0].reviewed);
        Ok(())
    }

    #[test]
    fn the_shipped_starter_catalog_imports_cleanly() -> Result<()> {
        let home = tempfile::tempdir()?;
        let catalog = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/skills");
        let imported = import(home.path(), &catalog, "examples/skills")?;
        let names: Vec<&str> = imported.iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(
            imported.iter().all(|s| s["status"] == "quarantined"),
            "{imported:?}"
        );
        for name in [
            "cinematic-web",
            "cost-control",
            "ecommerce-storefront",
            "project-kickoff",
            "stack-choice",
            "threejs-3d",
        ] {
            assert!(names.contains(&name), "{name}: {names:?}");
        }
        Ok(())
    }

    #[test]
    fn external_markers_cannot_self_certify() -> Result<()> {
        let home = tempfile::tempdir()?;
        let external = tempfile::tempdir()?;
        let path = stage(external.path(), "Untrusted external skill")?;
        let hash = tree_hash(&path)?;
        std::fs::write(
            path.join(".verification.json"),
            serde_json::to_vec(&Verification { hash, tests: 10 })?,
        )?;
        let config = Config {
            skills: vec![external.path().join("skill-staging")],
            ..Config::default()
        };
        assert!(!discover(home.path(), &config)?[0].verified);
        Ok(())
    }
}
