//! Effective, deny-by-default permissions. MCP annotations never confer authority.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
use anyhow::{Context, Result, ensure};
use layeragent_protocol::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[cfg(target_os = "linux")]
pub mod files;
pub mod mcp;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Read,
    Write,
    Send,
    Delete,
    Execute,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolGrant {
    pub operations: BTreeSet<Operation>,
    /// Relative directory prefixes. "." explicitly grants the entire project root.
    pub destinations: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpGrant {
    pub scope: Scope,
    pub project_id: String,
    pub root: PathBuf,
    pub revision: u64,
    pub catalog_sha256: String,
    pub tools: BTreeMap<String, ToolGrant>,
}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn file_catalog() -> Vec<Value> {
    let path = json!({"type":"string","minLength":1,"maxLength":4096});
    let hash = json!({"type":"string","pattern":"^[a-f0-9]{64}$"});
    [
        ("read_file", "Read a bounded UTF-8 slice and full file hash", json!({"path":path,"offset":{"type":"integer","minimum":0,"maximum":16777216},"limit":{"type":"integer","minimum":1,"maximum":65536}}), vec!["path"]),
        ("list_files", "List direct project directory entries", json!({"path":path}), vec!["path"]),
        ("write_file", "Create or replace a project file; replacement requires the observed hash", json!({"path":path,"content":{"type":"string","maxLength":1048576},"expected_sha256":hash}), vec!["path","content"]),
        ("edit_file", "Edit one unique fragment; requires the observed hash", json!({"path":path,"old":{"type":"string","minLength":1,"maxLength":1048576},"new":{"type":"string","maxLength":1048576},"expected_sha256":hash}), vec!["path","old","new","expected_sha256"]),
        ("delete_file", "Delete a regular project file; requires the observed hash", json!({"path":path,"expected_sha256":hash}), vec!["path","expected_sha256"]),
    ].into_iter().map(|(name,description,properties,required)| json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
pub fn catalog_hash() -> String {
    // Fixed, application-owned schema; never a server's self-declared behavior claim.
    digest(&serde_json::to_vec(&file_catalog()).expect("static catalog serializes"))
}
pub fn operation(tool: &str) -> Result<Operation> {
    Ok(match tool {
        "read_file" | "list_files" => Operation::Read,
        "write_file" | "edit_file" => Operation::Write,
        "delete_file" => Operation::Delete,
        _ => anyhow::bail!("unreviewed tool"),
    })
}
pub fn validate_path(path: &str, directory: bool) -> Result<()> {
    ensure!(
        path.len() <= 4096 && !path.contains('\0'),
        "invalid destination"
    );
    if directory && path == "." {
        return Ok(());
    }
    ensure!(
        !path.is_empty() && !path.starts_with('/'),
        "relative destination required"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty() && !matches!(part, "." | ".."),
            "invalid path component"
        );
        ensure!(
            !part.starts_with(".env")
                && !matches!(
                    part,
                    ".git"
                        | ".agents"
                        | ".layeragent"
                        | ".ssh"
                        | ".aws"
                        | ".gnupg"
                        | ".kube"
                        | ".netrc"
                        | ".npmrc"
                        | ".pypirc"
                        | ".git-credentials"
                ),
            "protected destination"
        );
    }
    Ok(())
}
impl McpGrant {
    pub fn validate(&self) -> Result<()> {
        layeragent_protocol::validate_name(&self.project_id)?;
        layeragent_protocol::validate_name(&self.scope.tenant)?;
        layeragent_protocol::validate_name(&self.scope.principal)?;
        ensure!(
            self.revision > 0 && self.root.is_absolute(),
            "invalid project grant"
        );
        ensure!(
            self.root != std::path::Path::new("/"),
            "filesystem root cannot be granted"
        );
        ensure!(
            !["/proc", "/sys", "/dev"]
                .iter()
                .any(|root| self.root.starts_with(root)),
            "virtual system roots cannot be granted"
        );
        ensure!(
            ![
                "/home", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/var", "/run", "/tmp"
            ]
            .iter()
            .any(|root| self.root == std::path::Path::new(root)),
            "grant a specific project directory, not a system-wide root"
        );
        ensure!(
            self.catalog_sha256 == catalog_hash(),
            "MCP catalog changed; owner review required"
        );
        for (tool, grant) in &self.tools {
            let required = operation(tool)?;
            ensure!(
                grant.operations == BTreeSet::from([required]),
                "tool grant must name its exact supported operation"
            );
            ensure!(!grant.destinations.is_empty(), "destination grant required");
            for prefix in &grant.destinations {
                validate_path(prefix, true)?;
            }
        }
        Ok(())
    }
    pub fn authorize(&self, scope: &Scope, tool: &str, arguments: &Value) -> Result<()> {
        self.validate()?;
        ensure!(&self.scope == scope, "MCP identity outside grant");
        let grant = self.tools.get(tool).context("tool outside grant")?;
        ensure!(
            grant.operations.contains(&operation(tool)?),
            "operation outside grant"
        );
        let schema = file_catalog()
            .into_iter()
            .find(|v| v["name"] == tool)
            .context("unreviewed tool")?;
        ensure!(
            jsonschema::validator_for(&schema["inputSchema"])?.is_valid(arguments),
            "arguments outside reviewed schema"
        );
        let path = arguments["path"].as_str().context("destination required")?;
        validate_path(path, tool == "list_files")?;
        ensure!(
            grant.destinations.iter().any(|p| p == "."
                || path == p
                || path
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with('/'))),
            "destination outside grant"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn grant(root: PathBuf) -> McpGrant {
        McpGrant {
            scope: Scope::local(),
            project_id: "project".into(),
            root,
            revision: 1,
            catalog_sha256: catalog_hash(),
            tools: BTreeMap::from([(
                "read_file".into(),
                ToolGrant {
                    operations: BTreeSet::from([Operation::Read]),
                    destinations: BTreeSet::from(["docs".into()]),
                },
            )]),
        }
    }
    #[test]
    fn permissions_are_exact_and_catalog_bound() {
        let mut g = grant(std::env::temp_dir().join("layeragent-policy-test"));
        assert!(
            g.authorize(&Scope::local(), "read_file", &json!({"path":"docs/a"}))
                .is_ok()
        );
        for path in ["docs2/a", "docs/../key", "docs/.env", "/docs/a"] {
            assert!(
                g.authorize(&Scope::local(), "read_file", &json!({"path":path}))
                    .is_err()
            );
        }
        assert!(
            g.authorize(
                &Scope::local(),
                "write_file",
                &json!({"path":"docs/a","content":"x"})
            )
            .is_err()
        );
        assert!(
            g.authorize(
                &Scope {
                    tenant: "other".into(),
                    principal: "owner".into()
                },
                "read_file",
                &json!({"path":"docs/a"})
            )
            .is_err()
        );
        g.catalog_sha256 = "0".repeat(64);
        assert!(g.validate().is_err());
    }
}
