//! Where skills come from besides the agent's verified drafts: catalogs the
//! operator imports and guides without fixtures. Every such skill waits in
//! quarantine, out of the agent's prompt, until the operator has read it and
//! accepted it.
use super::*;

/// Skills waiting for the operator, outside discovery.
pub(crate) const QUARANTINE: &str = "skill-quarantine";
/// Where an imported or drafted skill came from, kept with it.
pub(crate) const PROVENANCE: &str = ".import.json";
/// The operator's acceptance of a skill that has no fixtures.
pub(crate) const REVIEW: &str = ".review.json";
const MAX_FILES: usize = 200;
const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 5 * 1024 * 1024;
/// File kinds an operator should read before accepting a skill.
const SCRIPT_SUFFIXES: [&str; 9] = ["sh", "bash", "py", "js", "mjs", "ts", "rb", "pl", "ps1"];

#[derive(Serialize, Deserialize)]
struct Provenance {
    source: String,
    imported_at: i64,
    files: Vec<String>,
    scripts: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Review {
    pub(crate) hash: String,
    accepted_at: i64,
}

/// The skill directories below `source`: itself, or any directory up to
/// four levels down holding a `SKILL.md`.
fn skill_dirs(source: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .max_depth(5)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            e.depth() == 0 || !(name.starts_with('.') || name == "node_modules" || name == "target")
        })
    {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.file_name() == "SKILL.md"
            && let Some(parent) = entry.path().parent()
        {
            found.push(parent.to_path_buf());
        }
    }
    found.sort();
    Ok(found)
}

/// Copies one skill directory, refusing what a skill must not carry.
fn copy_checked(from: &Path, to: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let (mut files, mut scripts, mut total) = (Vec::new(), Vec::new(), 0u64);
    for entry in walkdir::WalkDir::new(from).follow_links(false) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(from)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        ensure!(!entry.path_is_symlink(), "skills cannot contain symlinks");
        for part in relative.components() {
            let name = part.as_os_str().to_string_lossy();
            ensure!(
                !layeragent_config::is_secret_path_component(&name)
                    && name != ".git"
                    && name != PROVENANCE
                    && name != REVIEW
                    && name != ".verification.json",
                "skill carries a refused path: {}",
                relative.display()
            );
        }
        let target = to.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        let size = entry.metadata()?.len();
        ensure!(
            size <= MAX_FILE_BYTES,
            "skill file too large: {}",
            relative.display()
        );
        total += size;
        ensure!(total <= MAX_TOTAL_BYTES, "skill too large");
        let bytes = std::fs::read(entry.path())?;
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let mut checked = json!(text);
            ensure!(
                layeragent_protocol::redact::redact_value(&mut checked) == 0,
                "skill file contains credentials: {}",
                relative.display()
            );
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Copied without execute bits: running anything is the agent's
        // explicit, confined choice.
        std::fs::write(&target, &bytes)?;
        let name = relative.to_string_lossy().into_owned();
        if relative
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SCRIPT_SUFFIXES.contains(&e))
        {
            scripts.push(name.clone());
        }
        files.push(name);
        ensure!(files.len() <= MAX_FILES, "skill has too many files");
    }
    Ok((files, scripts))
}

/// Puts a skill directory in quarantine under its frontmatter name.
pub(crate) fn quarantine(home: &Path, from: &Path, source: &str) -> Result<Value> {
    let raw = std::fs::read_to_string(from.join("SKILL.md"))?;
    ensure!(raw.len() <= 262_144, "skill is too large");
    let meta = parse_frontmatter(&raw)?;
    let root = home.join(QUARANTINE);
    private_dir(&root)?;
    let dest = root.join(&meta.name);
    let staging = root.join(format!(".{}-{}", meta.name, layeragent_protocol::new_id()));
    std::fs::create_dir(&staging)?;
    let copied = copy_checked(from, &staging);
    let (files, scripts) = match copied {
        Ok(copied) => copied,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    std::fs::write(
        staging.join(PROVENANCE),
        serde_json::to_vec_pretty(&Provenance {
            source: source.into(),
            imported_at: layeragent_protocol::now_ms(),
            files: files.clone(),
            scripts: scripts.clone(),
        })?,
    )?;
    // A newer import replaces an earlier one still waiting for review.
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::rename(&staging, &dest)?;
    Ok(
        json!({"name":meta.name,"description":meta.description,"files":files,"scripts":scripts,"status":"quarantined"}),
    )
}

/// Imports every skill found below `source` into quarantine. A skill that
/// cannot be imported is reported and the others still are.
pub fn import(home: &Path, source: &Path, label: &str) -> Result<Vec<Value>> {
    let _lease = registry_lease(home)?;
    ensure!(
        source.is_dir() && !source.is_symlink(),
        "import a directory"
    );
    let dirs = skill_dirs(source)?;
    ensure!(
        !dirs.is_empty(),
        "no SKILL.md found below {}",
        source.display()
    );
    Ok(dirs
        .iter()
        .map(|dir| {
            let label = format!("{label}:{}", dir.strip_prefix(source).unwrap_or(dir).display());
            quarantine(home, dir, &label).unwrap_or_else(|error| {
                json!({"path":dir.strip_prefix(source).unwrap_or(dir),"status":"refused","reason":error.to_string()})
            })
        })
        .collect())
}

/// The skills waiting for the operator.
pub fn quarantined(home: &Path) -> Result<Vec<Value>> {
    let root = home.join(QUARANTINE);
    let mut found = Vec::new();
    if !root.is_dir() {
        return Ok(found);
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !path.is_dir() || path.is_symlink() || name.starts_with('.') {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(path.join("SKILL.md")) else {
            continue;
        };
        let Ok(meta) = parse_frontmatter(&raw) else {
            continue;
        };
        let provenance: Option<Provenance> = std::fs::read(path.join(PROVENANCE))
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok());
        found.push(json!({
            "name": meta.name,
            "description": meta.description,
            "source": provenance.as_ref().map(|p| p.source.clone()),
            "files": provenance.as_ref().map(|p| p.files.clone()).unwrap_or_default(),
            "scripts": provenance.as_ref().map(|p| p.scripts.clone()).unwrap_or_default(),
            "hash": tree_hash(&path)?,
        }));
    }
    found.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(found)
}

fn waiting(home: &Path, name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    let path = home.join(QUARANTINE).join(name);
    ensure!(
        path.is_dir() && !path.is_symlink(),
        "no quarantined skill named {name}; layeragent skills quarantine lists them"
    );
    Ok(path)
}

/// Everything the operator should read before accepting a skill.
pub fn review(home: &Path, name: &str) -> Result<Value> {
    let path = waiting(home, name)?;
    let body = std::fs::read_to_string(path.join("SKILL.md"))?;
    let meta = parse_frontmatter(&body)?;
    let provenance: Option<Provenance> = std::fs::read(path.join(PROVENANCE))
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok());
    let links: Vec<&str> = body
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '"' | '\''))
        .filter(|word| word.starts_with("http://") || word.starts_with("https://"))
        .collect();
    Ok(json!({
        "name": meta.name,
        "description": meta.description,
        "source": provenance.as_ref().map(|p| p.source.clone()),
        "files": provenance.as_ref().map(|p| p.files.clone()).unwrap_or_default(),
        "scripts": provenance.as_ref().map(|p| p.scripts.clone()).unwrap_or_default(),
        "links": links,
        "hash": tree_hash(&path)?,
        "skill_md": body,
    }))
}

/// Moves a quarantined skill into the active registry, archiving an active
/// version with the same name, and records the acceptance.
pub fn accept(home: &Path, name: &str) -> Result<Value> {
    let _lease = registry_lease(home)?;
    let source = waiting(home, name)?;
    let _ = std::fs::remove_file(source.join(PROVENANCE));
    let hash = tree_hash(&source)?;
    std::fs::write(
        source.join(REVIEW),
        serde_json::to_vec(&Review {
            hash: hash.clone(),
            accepted_at: layeragent_protocol::now_ms(),
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
        if !archived.exists() {
            std::fs::rename(&active, &archived)?;
        } else {
            std::fs::remove_dir_all(&active)?;
        }
        previous = Some(archived);
    }
    if let Err(error) = std::fs::rename(&source, &active) {
        if let Some(previous) = previous.filter(|p| p.exists()) {
            std::fs::rename(previous, &active).context("restoring the previous skill")?;
        }
        return Err(error.into());
    }
    Ok(json!({"skill":name,"hash":hash,"active":true,"reviewed":true}))
}

/// Drops a quarantined skill.
pub fn reject(home: &Path, name: &str) -> Result<()> {
    let _lease = registry_lease(home)?;
    std::fs::remove_dir_all(waiting(home, name)?)?;
    Ok(())
}

/// Active and quarantined skills matching `query`, best first: words in the
/// name count most, then the description, then the body.
pub fn search(home: &Path, config: &Config, query: &str, limit: usize) -> Result<Vec<Value>> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.chars().count() >= 2)
        .map(str::to_string)
        .take(12)
        .collect();
    ensure!(!terms.is_empty(), "query needs at least one word");
    let count = |text: &str, term: &str| text.matches(term).count();
    let mut scored: Vec<(usize, Value)> = Vec::new();
    for skill in discover(home, config)? {
        let body = load(&skill).unwrap_or_default().to_lowercase();
        let (name, description) = (skill.name.to_lowercase(), skill.description.to_lowercase());
        let score: usize = terms
            .iter()
            .map(|t| count(&name, t) * 5 + count(&description, t) * 3 + count(&body, t).min(5))
            .sum();
        if score == 0 {
            continue;
        }
        let snippet = body
            .lines()
            .find(|line| terms.iter().any(|t| line.contains(t.as_str())))
            .map(|line| line.trim().chars().take(200).collect::<String>());
        let status = if skill.verified {
            "verified"
        } else if skill.reviewed {
            "reviewed"
        } else {
            "unverified"
        };
        scored.push((score, json!({"name":skill.name,"description":skill.description,"status":status,"score":score,"snippet":snippet})));
    }
    for candidate in quarantined(home)? {
        let name = candidate["name"].as_str().unwrap_or("").to_lowercase();
        let description = candidate["description"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        let score: usize = terms
            .iter()
            .map(|t| count(&name, t) * 5 + count(&description, t) * 3)
            .sum();
        if score > 0 {
            scored.push((score, json!({"name":candidate["name"],"description":candidate["description"],"status":"quarantined","score":score,"note":"waiting for the operator: layeragent skills review then accept"})));
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1["name"].as_str().cmp(&b.1["name"].as_str()))
    });
    Ok(scored
        .into_iter()
        .take(limit.clamp(1, 20))
        .map(|(_, v)| v)
        .collect())
}
