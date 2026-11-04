//! Planning mode: the agent investigates and writes the plan, then asks the
//! operator to approve it. Nothing is built in this mode.
use layeragent_config::Config;

/// The only directory file tools may write to while planning.
pub(crate) const WRITE_SCOPE: &str = "docs/plan";

/// Tools a planning turn keeps: reading, searching, research in the agent's
/// own browser, memory, skills, budget and read-only delegation, plus file
/// writes limited to [`WRITE_SCOPE`]. Shell, terminals, Git writes, MCP calls,
/// custom tools and skill authoring are left out.
const PLAN_TOOLS: [&str; 33] = [
    "context_status",
    "compact_context",
    "budget_status",
    "read_file",
    "list_files",
    "search_files",
    "write_file",
    "edit_file",
    "git_status",
    "git_diff",
    "memory_search",
    "memory_remember",
    "memory_revise",
    "memory_relate",
    "memory_related",
    "artifact_read",
    "skill_load",
    "skill_search",
    "mcp_list",
    "mcp_read_resource",
    "record_unknown",
    "browser_open",
    "browser_search",
    "browser_snapshot",
    "browser_scroll",
    "browser_wait",
    "browser_screenshot",
    "browser_console",
    "browser_close",
    "spawn_agent",
    "wait_agent",
    "cancel_agent",
    "list_agents",
];

/// The configuration of a planning turn.
pub(crate) fn restrict(mut config: Config) -> Config {
    config.policy.shell = false;
    let allowed: Vec<String> = if config.policy.allowed_tools.is_empty() {
        PLAN_TOOLS.iter().map(|tool| tool.to_string()).collect()
    } else {
        config
            .policy
            .allowed_tools
            .iter()
            .filter(|tool| PLAN_TOOLS.contains(&tool.as_str()))
            .cloned()
            .collect()
    };
    config.policy.allowed_tools = if allowed.is_empty() {
        // An empty list means every tool; keep it meaning none.
        vec!["<none>".into()]
    } else {
        allowed
    };
    config.tools.clear();
    config.auto_skills = false;
    config
}

/// What a planning turn is asked to produce.
pub(crate) const INSTRUCTIONS: &str = "\n\nPlanning mode (set by the operator): do not build anything yet. Investigate what you need, then write the plan as Markdown under docs/plan/: brief.md (the goal, who it is for, what success looks like, what is out of scope, and open questions at the end), stack.md (the stack you propose and why, fitted to the operator's profile, with a simpler and a more capable alternative and their trade-offs), plan.md (ordered steps, each with its deliverable and how it will be checked), acceptance.md (acceptance criteria and the checks that prove them, including visual checks for anything people will see) and costs.md (estimated tokens and cost per step at the configured prices; budget_status lists them). Decide what you reasonably can and ask only what you cannot. Shell is unavailable here and file writes are limited to docs/plan/. End with a short summary and ask the operator to approve the plan or say what to change; nothing is built until they do.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_planning_turn_reads_researches_and_writes_only_its_plan() {
        let mut config = Config::default();
        config.policy.shell = true;
        config.tools.insert(
            "deploy".into(),
            serde_json::from_value(
                serde_json::json!({"command":["deploy"],"description":"deploy","parameters":{"type":"object"}}),
            )
            .expect("custom tool"),
        );
        let planned = restrict(config.clone());
        assert!(!planned.policy.shell);
        assert!(planned.tools.is_empty(), "custom tools can have effects");
        for tool in ["write_file", "browser_open", "spawn_agent", "budget_status"] {
            assert!(
                planned.policy.allowed_tools.contains(&tool.to_string()),
                "{tool}"
            );
        }
        for tool in [
            "shell",
            "pty_start",
            "git_commit",
            "mcp_call",
            "skill_draft",
        ] {
            assert!(
                !planned.policy.allowed_tools.contains(&tool.to_string()),
                "{tool}"
            );
        }
        // An operator allow-list is narrowed, never widened.
        config.policy.allowed_tools = vec!["read_file".into(), "shell".into()];
        assert_eq!(restrict(config.clone()).policy.allowed_tools, ["read_file"]);
        config.policy.allowed_tools = vec!["shell".into()];
        assert_eq!(restrict(config).policy.allowed_tools, ["<none>"]);
    }
}
