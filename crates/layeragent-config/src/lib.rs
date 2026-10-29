use anyhow::{Context, Result, ensure};
use layeragent_protocol::validate_name;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub primary: String,
    pub fallbacks: Vec<String>,
    pub providers: BTreeMap<String, Provider>,
    pub models: BTreeMap<String, Model>,
    pub policy: Policy,
    pub instructions: String,
    pub skills: Vec<PathBuf>,
    pub tools: BTreeMap<String, CustomTool>,
    pub mcp: BTreeMap<String, McpServer>,
    pub channels: BTreeMap<String, Channel>,
    pub access: Vec<Access>,
    pub max_rounds: usize,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    pub gateway: Gateway,
    pub hooks: BTreeMap<String, Vec<String>>,
    pub memory: MemoryConfig,
    pub max_cost_usd: Option<f64>,
    pub auto_skills: bool,
    pub default_effort: layeragent_protocol::EffortLevel,
    /// Models and native effort levels explicitly qualified by the operator.
    pub effort_models: BTreeMap<String, Vec<layeragent_protocol::EffortLevel>>,
    pub memory_recall_limit: usize,
    pub memory_max_bytes: usize,
    pub agents: AgentsConfig,
    pub browser: BrowserConfig,
    pub privacy: Privacy,
    /// The encrypted remote bridge of this profile, when it is configured.
    #[serde(default)]
    pub remote: Option<RemoteBridge>,
}

/// Location of the encrypted remote bridge.
///
/// Only the owner client configuration is referenced. The daemon reads the
/// public job metadata from it and never receives a pairing key.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RemoteBridge {
    /// Path of the owner client configuration of the bridge.
    pub client: PathBuf,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            primary: String::new(),
            fallbacks: vec![],
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            policy: Policy::default(),
            instructions: String::new(),
            skills: vec![],
            tools: BTreeMap::new(),
            mcp: BTreeMap::new(),
            channels: BTreeMap::new(),
            access: vec![],
            max_rounds: 32,
            max_input_tokens: 200_000,
            max_output_tokens: 16_384,
            gateway: Gateway::default(),
            hooks: BTreeMap::new(),
            memory: MemoryConfig::default(),
            max_cost_usd: None,
            auto_skills: true,
            default_effort: layeragent_protocol::EffortLevel::Auto,
            effort_models: BTreeMap::new(),
            memory_recall_limit: 5,
            memory_max_bytes: 16_384,
            agents: AgentsConfig::default(),
            browser: BrowserConfig::default(),
            privacy: Privacy::default(),
            remote: None,
        }
    }
}

/// Outbound model policy. An empty allowlist permits explicitly configured
/// providers; local_models_only forbids remote endpoints, including fallbacks.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Privacy {
    pub local_models_only: bool,
    pub allowed_providers: Vec<String>,
}
/// The agent's own browser. By default it launches an isolated Chromium (found on the
/// system or downloaded once from Chrome for Testing) whose traffic goes through the
/// confined proxy, so only `allow_domains` and never private addresses are reachable.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserConfig {
    pub enabled: bool,
    /// Explicit Chrome/Chromium executable; skips discovery and download.
    pub executable: Option<PathBuf>,
    /// Download a managed Chromium the first time it is needed and none is found.
    pub auto_install: bool,
    /// Reserved: visible mode is currently rejected until display access is isolated.
    pub visible: bool,
    /// Reachable hosts: `example.com`, `*.example.com` or `*` (any public host).
    pub allow_domains: Vec<String>,
    /// Deprecated unsafe option: true is rejected; Chromium's sandbox is mandatory.
    pub no_sandbox: bool,
    /// DevTools endpoint of a browser you started yourself with --remote-debugging-port.
    /// Reserved: host-browser attachment is currently rejected by the hardened backend.
    pub attach_url: Option<String>,
    /// Search page template containing `{query}` (for example a SearXNG instance). Without
    /// it, `browser_search` reads Bing's results page.
    pub search_url: Option<String>,
    pub page_timeout_secs: u64,
}
impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            executable: None,
            auto_install: true,
            visible: false,
            allow_domains: vec!["*".into()],
            no_sandbox: false,
            attach_url: None,
            search_url: None,
            page_timeout_secs: 30,
        }
    }
}
/// Delegation to child agents. Limits apply to the whole tree rooted at a user turn.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct AgentsConfig {
    pub enabled: bool,
    /// Levels of children below the user's turn (1 = children cannot delegate further).
    pub max_depth: usize,
    /// Children running at the same time; the rest wait for a slot.
    pub max_concurrent: usize,
    pub max_children_per_turn: usize,
    pub child_max_rounds: usize,
    pub child_timeout_secs: u64,
    pub child_max_output_tokens: u64,
    /// Input+output tokens shared by the root and all of its descendants.
    pub tree_max_tokens: Option<u64>,
    pub tree_max_cost_usd: Option<f64>,
}
impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_depth: 1,
            max_concurrent: 4,
            max_children_per_turn: 8,
            child_max_rounds: 16,
            child_timeout_secs: 900,
            child_max_output_tokens: 8192,
            tree_max_tokens: Some(1_500_000),
            tree_max_cost_usd: None,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryConfig {
    pub embeddings: bool,
    pub model: String,
    pub idle_seconds: u64,
}
impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            embeddings: false,
            model: "multilingual-e5-small".into(),
            idle_seconds: 120,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Wire {
    OpenaiChat,
    OpenaiResponses,
    Anthropic,
    Gemini,
    Ollama,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub wire: Wire,
    pub base_url: String,
    #[serde(default)]
    pub key_env: Option<String>,
    #[serde(default)]
    pub oauth: Option<OAuth>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OAuth {
    pub client_id: String,
    pub token_url: String,
    pub authorization_url: Option<String>,
    pub device_url: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Model {
    pub provider: String,
    pub id: String,
    pub context_window: u64,
    #[serde(default = "yes")]
    pub tools: bool,
    #[serde(default)]
    pub emulate_tools: bool,
    #[serde(default)]
    pub input_per_million: Option<f64>,
    #[serde(default)]
    pub output_per_million: Option<f64>,
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub shell: bool,
    pub write: bool,
    pub network: bool,
    pub allowed_tools: Vec<String>,
    pub approval_tools: Vec<String>,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    /// Extra absolute directories mounted read-only inside the sandbox.
    pub read_roots: Vec<PathBuf>,
    /// Extra absolute directories mounted writable (in addition to the workspace when `write`).
    pub write_roots: Vec<PathBuf>,
    /// Names or workspace-relative paths forced read-only everywhere, on top of `.git`,
    /// `.agents` and `.layeragent`.
    pub protected_paths: Vec<String>,
    pub limits: Limits,
    /// Hosts reachable through the confined proxy when `network` is true:
    /// `example.com`, `*.example.com` or `*` (any public host). Empty means no access.
    pub allow_domains: Vec<String>,
}
/// Per-execution resource ceilings. `None` keeps the platform default.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub memory_bytes: Option<u64>,
    pub max_pids: Option<u64>,
    pub cpu_secs: Option<u64>,
    pub file_size_bytes: Option<u64>,
    pub open_files: Option<u64>,
    /// Also enforce memory/pids/cpu through a cgroup v2 leaf when the host delegates one.
    /// Fails the execution if requested and unavailable.
    pub cgroup: bool,
    /// CPU quota in percent of one core (cgroup only).
    pub cpu_percent: Option<u32>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: None,
            max_pids: None,
            cpu_secs: Some(120),
            file_size_bytes: Some(256 * 1024 * 1024),
            open_files: Some(256),
            cgroup: false,
            cpu_percent: None,
        }
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            shell: true,
            write: true,
            network: false,
            allowed_tools: vec![],
            approval_tools: vec![],
            timeout_secs: 300,
            max_output_bytes: 1_048_576,
            read_roots: vec![],
            write_roots: vec![],
            protected_paths: vec![],
            limits: Limits::default(),
            allow_domains: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CustomTool {
    pub description: String,
    pub parameters: serde_json::Value,
    pub command: Vec<String>,
    #[serde(default)]
    pub approval: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Option<Vec<String>>,
    pub url: Option<String>,
    #[serde(default)]
    pub enabled: bool,
    /// Only adapters with effective service-side restrictions may be enabled.
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub grant: Option<layeragent_policy::McpGrant>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    pub kind: String,
    pub token_env: String,
    pub secret_env: Option<String>,
    #[serde(default)]
    pub verify_token_env: Option<String>,
    pub endpoint: Option<String>,
    pub workspace: PathBuf,
    #[serde(default = "webhook_mode")]
    pub mode: String,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub intents: Option<u64>,
}
fn webhook_mode() -> String {
    "webhook".into()
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct Access {
    pub token_env: String,
    pub tenant: String,
    pub principal: String,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub workspaces: Vec<PathBuf>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub sender_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Gateway {
    pub bind: String,
    pub token_env: String,
}
impl Default for Gateway {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:9473".into(),
            token_env: "LAYERAGENT_TOKEN".into(),
        }
    }
}

impl Config {
    pub fn require_model_egress(&self, alias: &str) -> Result<()> {
        let model = self
            .models
            .get(alias)
            .context("selected model is not configured")?;
        ensure!(
            self.privacy.allowed_providers.is_empty()
                || self.privacy.allowed_providers.contains(&model.provider),
            "provider is outside the outbound allowlist"
        );
        let provider = self
            .providers
            .get(&model.provider)
            .context("provider is missing")?;
        let authority = provider
            .base_url
            .split("://")
            .nth(1)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        let local = ["localhost", "127.0.0.1", "[::1]"].iter().any(|host| {
            authority == *host
                || authority
                    .strip_prefix(&format!("{host}:"))
                    .is_some_and(|port| port.parse::<u16>().is_ok())
        });
        ensure!(
            !self.privacy.local_models_only || local,
            "privacy.local_models_only forbids remote model requests"
        );
        Ok(())
    }
    /// Register referenced credentials before any user content is persisted or sent.
    /// Values stay in process memory and never become part of this configuration.
    pub fn register_secrets(&self) {
        let register_env = |name: &str| {
            if let Ok(value) = std::env::var(name) {
                layeragent_protocol::redact::register_secret(&value);
            }
        };
        register_env(&self.gateway.token_env);
        for provider in self.providers.values() {
            if let Some(name) = &provider.key_env {
                register_env(name);
            }
            for (name, value) in &provider.headers {
                let name = name.to_ascii_lowercase();
                if ["token", "secret", "key", "auth", "cookie"]
                    .iter()
                    .any(|part| name.contains(part))
                {
                    layeragent_protocol::redact::register_secret(value);
                }
            }
        }
        for access in &self.access {
            register_env(&access.token_env);
        }
        for channel in self.channels.values() {
            register_env(&channel.token_env);
            for name in [&channel.secret_env, &channel.verify_token_env]
                .into_iter()
                .flatten()
            {
                register_env(name);
            }
        }
    }
    pub fn load(home: &Path, profile: &str) -> Result<Self> {
        validate_name(profile)?;
        let base = home.join("config.toml");
        let mut value: toml::Value = toml::Value::try_from(Self::default())?;
        if base.exists() {
            merge(
                &mut value,
                toml::from_str(&std::fs::read_to_string(&base).context("reading config")?)
                    .context("invalid config TOML")?,
            );
        }
        let path = home.join("profiles").join(format!("{profile}.toml"));
        if path.exists() {
            merge(&mut value, toml::from_str(&std::fs::read_to_string(path)?)?);
        }
        let config: Self = value.try_into().context("invalid configuration fields")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        if let Some(cap) = self.max_cost_usd {
            ensure!(
                cap.is_finite() && cap > 0.0,
                "max_cost_usd must be positive"
            );
        }
        ensure!(self.schema_version == 1, "unsupported configuration schema");
        ensure!(
            self.max_rounds > 0 && self.max_rounds <= 1024,
            "max_rounds must be 1..1024"
        );
        ensure!(
            self.max_input_tokens > 0 && self.max_output_tokens > 0,
            "token budgets must be positive"
        );
        ensure!(
            self.memory_recall_limit > 0 && self.memory_recall_limit <= 50,
            "memory_recall_limit must be 1..50"
        );
        ensure!(
            (1024..=262_144).contains(&self.memory_max_bytes),
            "memory_max_bytes must be 1024..262144"
        );
        let agents = &self.agents;
        ensure!(
            agents.max_depth <= 16,
            "agents.max_depth must not exceed 16"
        );
        ensure!(
            agents.max_concurrent > 0 && agents.max_concurrent <= 64,
            "agents.max_concurrent must be 1..64"
        );
        ensure!(
            agents.max_children_per_turn > 0 && agents.max_children_per_turn <= 256,
            "agents.max_children_per_turn must be 1..256"
        );
        ensure!(
            agents.child_max_rounds > 0 && agents.child_max_rounds <= 1024,
            "agents.child_max_rounds must be 1..1024"
        );
        ensure!(
            agents.child_timeout_secs > 0 && agents.child_timeout_secs <= 86_400,
            "agents.child_timeout_secs must be 1..86400"
        );
        ensure!(
            agents.child_max_output_tokens > 0,
            "agents.child_max_output_tokens must be positive"
        );
        ensure!(
            agents.tree_max_tokens.is_none_or(|v| v > 0),
            "agents.tree_max_tokens must be positive"
        );
        ensure!(
            agents
                .tree_max_cost_usd
                .is_none_or(|v| v.is_finite() && v > 0.0),
            "agents.tree_max_cost_usd must be finite and positive"
        );
        for (model, levels) in &self.effort_models {
            ensure!(
                self.models.contains_key(model),
                "effort model {model} is not configured"
            );
            ensure!(
                !levels.is_empty()
                    && levels
                        .iter()
                        .all(|level| *level != layeragent_protocol::EffortLevel::Auto),
                "effort_models must declare concrete levels"
            );
        }
        ensure!(
            self.policy.timeout_secs > 0 && self.policy.max_output_bytes >= 1024,
            "invalid execution limits"
        );
        for root in self
            .policy
            .read_roots
            .iter()
            .chain(&self.policy.write_roots)
        {
            ensure!(
                root.is_absolute() && root.parent().is_some(),
                "policy roots must be absolute and not the filesystem root"
            );
        }
        for path in &self.policy.protected_paths {
            ensure!(
                !path.is_empty() && !path.split('/').any(|part| part == ".."),
                "protected paths cannot be empty or contain .."
            );
        }
        for domain in &self.policy.allow_domains {
            ensure!(
                domain == "*"
                    || (!domain.is_empty()
                        && domain
                            .trim_start_matches("*.")
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')),
                "invalid allow_domains entry: {domain}"
            );
        }
        ensure!(
            !self.policy.network || !self.policy.allow_domains.is_empty(),
            "policy.network requires allow_domains; an empty list would grant nothing"
        );
        for (name, p) in &self.providers {
            validate_name(name)?;
            let url = &p.base_url;
            ensure!(
                url.starts_with("https://")
                    || url.starts_with("http://localhost:")
                    || url.starts_with("http://127.0.0.1:")
                    || url.starts_with("http://[::1]:"),
                "provider {name}: HTTPS required except loopback"
            );
            ensure!(
                !url.contains('@') && !url.contains('#') && !url.contains('?'),
                "provider URL must not include credentials, fragments or queries"
            );
            for key in p.headers.keys() {
                ensure!(
                    !["authorization", "x-api-key", "host", "cookie"]
                        .contains(&key.to_lowercase().as_str()),
                    "use secret references for authentication"
                );
            }
            if let Some(o) = &p.oauth {
                ensure!(
                    o.token_url.starts_with("https://"),
                    "OAuth token endpoint must use HTTPS"
                );
            }
        }
        for (name, m) in &self.models {
            for key in m.parameters.keys() {
                ensure!(
                    ![
                        "model",
                        "messages",
                        "input",
                        "contents",
                        "stream",
                        "tools",
                        "system",
                        "systemInstruction",
                        "api_key",
                        "authorization"
                    ]
                    .contains(&key.as_str()),
                    "model parameters cannot override request content or credentials"
                );
            }
            validate_name(name)?;
            ensure!(
                self.providers.contains_key(&m.provider),
                "model {name}: missing provider"
            );
            ensure!(
                m.context_window >= 1024 && !m.id.is_empty(),
                "model {name}: invalid model descriptor"
            );
            for price in [m.input_per_million, m.output_per_million]
                .into_iter()
                .flatten()
            {
                ensure!(price.is_finite() && price >= 0.0, "invalid model price");
            }
        }
        for id in std::iter::once(&self.primary)
            .chain(self.fallbacks.iter())
            .filter(|s| !s.is_empty())
        {
            ensure!(
                self.models.contains_key(id),
                "unknown configured model {id}"
            );
        }
        for a in &self.access {
            validate_name(&a.tenant)?;
            validate_name(&a.principal)?;
            ensure!(
                !(a.tenant == "default" && a.principal == "owner"),
                "default/owner is reserved for the runtime owner token"
            );
            ensure!(
                a.token_env != self.gateway.token_env,
                "access tokens cannot reuse the owner token reference"
            );
            for other in &self.access {
                if std::ptr::eq(a, other) {
                    continue;
                }
                if a.tenant == other.tenant && a.principal == other.principal {
                    let mut roots = a.workspaces.clone();
                    roots.sort();
                    roots.dedup();
                    let mut other_roots = other.workspaces.clone();
                    other_roots.sort();
                    other_roots.dedup();
                    ensure!(
                        a.read_only == other.read_only && roots == other_roots,
                        "bindings for the same identity must grant identical permissions"
                    );
                } else {
                    ensure!(
                        a.token_env != other.token_env,
                        "different identities cannot share a token reference"
                    );
                }
            }
        }
        for (name, c) in &self.channels {
            validate_name(name)?;
            ensure!(
                ["webhook", "native"].contains(&c.mode.as_str()),
                "channel mode must be webhook or native"
            );
            ensure!(
                [
                    "telegram", "discord", "slack", "whatsapp", "signal", "matrix", "webhook"
                ]
                .contains(&c.kind.as_str()),
                "unknown channel kind"
            );
            if let Some(url) = &c.endpoint {
                ensure!(
                    url.starts_with("https://")
                        || url.starts_with("http://127.0.0.1:")
                        || (c.kind == "signal" && url.starts_with("unix:/")),
                    "channel endpoint requires HTTPS, loopback, or a Signal UNIX socket"
                );
            }
        }
        for (name, server) in &self.mcp {
            validate_name(name)?;
            if server.enabled {
                ensure!(
                    server.adapter.as_deref() == Some("project-files"),
                    "MCP {name}: verified adapter required; legacy integrations need review"
                );
                ensure!(
                    server.command.is_none() && server.url.is_none(),
                    "MCP {name}: project-files cannot override transport"
                );
                server
                    .grant
                    .as_ref()
                    .context("MCP project grant required")?
                    .validate()?;
            }
        }
        for (name, t) in &self.tools {
            validate_name(name)?;
            ensure!(!t.command.is_empty(), "custom tool requires command");
        }
        Ok(())
    }
    pub fn revision(&self) -> String {
        hex::encode(Sha256::digest(serde_json::to_vec(self).unwrap_or_default()))
    }
    pub fn schema() -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(Config)).unwrap_or_default()
    }
}
fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (k, v) in overlay {
                if let Some(old) = base.get_mut(&k) {
                    merge(old, v);
                } else {
                    base.insert(k, v);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}
pub fn default_home() -> PathBuf {
    std::env::var_os("LAYERAGENT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            directories::BaseDirs::new()
                .map(|p| p.config_dir().join("layeragent"))
                .unwrap_or_else(|| PathBuf::from(".layeragent"))
        })
}
pub fn is_secret_path_component(name: &str) -> bool {
    name.starts_with(".env")
        || [
            ".layeragent",
            ".ssh",
            ".aws",
            ".gnupg",
            ".kube",
            ".netrc",
            ".npmrc",
            ".pypirc",
            ".git-credentials",
        ]
        .contains(&name)
}
pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outbound_policy_and_owner_identity_are_enforced() -> Result<()> {
        let mut config = Config::default();
        config.providers.insert(
            "local".into(),
            Provider {
                wire: Wire::Ollama,
                base_url: "http://127.0.0.1:11434".into(),
                key_env: None,
                oauth: None,
                headers: Default::default(),
            },
        );
        config.models.insert(
            "test".into(),
            Model {
                provider: "local".into(),
                id: "test".into(),
                context_window: 32768,
                tools: true,
                emulate_tools: false,
                input_per_million: None,
                output_per_million: None,
                parameters: Default::default(),
            },
        );
        config.privacy.local_models_only = true;
        config.require_model_egress("test")?;
        config
            .providers
            .get_mut("local")
            .context("provider")?
            .base_url = "https://remote.example.test".into();
        assert!(config.require_model_egress("test").is_err());
        config.privacy.local_models_only = false;
        config.privacy.allowed_providers = vec!["other".into()];
        assert!(config.require_model_egress("test").is_err());
        config.access.push(Access {
            token_env: "DELEGATED".into(),
            tenant: "default".into(),
            principal: "owner".into(),
            read_only: false,
            workspaces: vec![],
            platform: None,
            sender_id: None,
            channel_id: None,
        });
        assert!(config.validate().is_err());
        Ok(())
    }
    #[test]
    fn layered_profiles_and_unknown_keys() -> Result<()> {
        let d = tempfile::tempdir()?;
        std::fs::create_dir(d.path().join("profiles"))?;
        std::fs::write(
            d.path().join("config.toml"),
            "max_rounds = 8\n[policy]\nnetwork=false\n",
        )?;
        std::fs::write(
            d.path().join("profiles/dev.toml"),
            "[policy]\nwrite=false\n",
        )?;
        let c = Config::load(d.path(), "dev")?;
        assert_eq!(c.max_rounds, 8);
        assert!(!c.policy.write);
        assert!(!c.policy.network);
        std::fs::write(d.path().join("config.toml"), "unknown_key=true")?;
        assert!(Config::load(d.path(), "dev").is_err());
        Ok(())
    }
}
