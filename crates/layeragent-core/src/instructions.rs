//! Hierarchical project instruction loading (AGENTS.md / GEMINI.md).
//!
//! Files are read from the repository root down to the workspace, so the most
//! specific file comes last and takes precedence. Nested files below the
//! workspace are only advertised, because they apply to the files under them.
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const NAMES: [&str; 2] = ["AGENTS.md", "GEMINI.md"];
const FILE_LIMIT: usize = 65_536;
const TOTAL_LIMIT: usize = 131_072;
const NESTED_DEPTH: usize = 4;
const NESTED_MAX: usize = 20;

/// Nearest ancestor (inclusive) holding `.git`; without one only the workspace is used.
fn project_root(workspace: &Path) -> PathBuf {
    workspace
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(workspace)
        .to_path_buf()
}

fn truncate_utf8(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated: instruction file exceeds size limit]");
    }
    text
}

pub fn project_instructions(workspace: &Path) -> Result<Vec<String>> {
    let root = project_root(workspace);
    let mut dirs: Vec<&Path> = workspace
        .ancestors()
        .take_while(|dir| dir.starts_with(&root))
        .collect();
    dirs.reverse();
    let mut seen = HashSet::new();
    let mut total = 0;
    let mut parts = Vec::new();
    for dir in dirs {
        for name in NAMES {
            let path = dir.join(name);
            if !path.is_file() || path.is_symlink() {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            if !seen.insert(Sha256::digest(&bytes)) {
                continue;
            }
            let text = truncate_utf8(String::from_utf8_lossy(&bytes).into_owned(), FILE_LIMIT);
            if total + text.len() > TOTAL_LIMIT {
                parts.push(format!(
                    "Instructions from {} omitted: total instruction budget reached.",
                    path.display()
                ));
                continue;
            }
            total += text.len();
            parts.push(format!(
                "Instructions from {} (applies to {}; later, more specific files take precedence):\n{text}",
                path.display(),
                dir.display()
            ));
        }
    }
    let nested = nested_files(workspace);
    if !nested.is_empty() {
        parts.push(format!(
            "Scoped instruction files exist below the workspace. Read the relevant one with read_file before editing files under its directory:\n{}",
            nested.join("\n")
        ));
    }
    Ok(parts)
}

fn nested_files(workspace: &Path) -> Vec<String> {
    walkdir::WalkDir::new(workspace)
        .max_depth(NESTED_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                ".git" | "node_modules" | "target" | ".venv" | "venv" | "__pycache__"
            )
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.depth() > 1
                && e.file_type().is_file()
                && NAMES.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .take(NESTED_MAX)
        .filter_map(|e| {
            e.path()
                .strip_prefix(workspace)
                .ok()
                .map(|p| p.display().to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_root_to_workspace_dedupes_and_advertises_nested() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git"))?;
        std::fs::create_dir_all(root.join("app/sub"))?;
        std::fs::write(root.join("AGENTS.md"), "root rule")?;
        std::fs::write(root.join("app/AGENTS.md"), "app rule")?;
        std::fs::write(root.join("app/GEMINI.md"), "root rule")?; // duplicate content
        std::fs::write(root.join("app/sub/AGENTS.md"), "sub rule")?;
        let parts = project_instructions(&root.join("app"))?;
        assert!(parts[0].contains("root rule"));
        assert!(parts[1].contains("app rule"));
        assert_eq!(parts.iter().filter(|p| p.contains("root rule")).count(), 1);
        assert!(parts.last().is_some_and(|p| p.contains("sub/AGENTS.md")));
        Ok(())
    }

    #[test]
    fn no_git_root_means_workspace_only_and_oversize_is_truncated() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("AGENTS.md"), "x".repeat(FILE_LIMIT + 10))?;
        let parts = project_instructions(dir.path())?;
        assert_eq!(
            parts.len(),
            1,
            "{:?}",
            parts
                .iter()
                .map(|p| p.chars().take(120).collect::<String>())
                .collect::<Vec<_>>()
        );
        assert!(parts[0].contains("truncated"));
        Ok(())
    }
}
