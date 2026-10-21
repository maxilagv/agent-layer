# 06 · Context, instructions and token budgets

The prompt compiler combines runtime rules, configured instructions, principal-local instructions, project rules, skill metadata and retrieved historical facts. Retrieved content and tool output are reference material; they cannot grant authority.

## Project rules

`AGENTS.md` and `GEMINI.md` load from the nearest Git root to the workspace, with specific directories last. Content hashes deduplicate repeated instructions. Each file is capped at 64 KiB and the total at 128 KiB. Nested rule files are advertised for on-demand reading. Without a Git ancestor, only workspace-local rules are loaded.

## Budgeting

| Control | Behavior |
| --- | --- |
| Input estimate | Conservative estimate with an 80% model-window margin |
| Output ceiling | Remaining configured output budget |
| Turn rounds | Bound on repeated tool batches |
| Monetary ceiling | Estimated reservation requires configured model prices |
| Child tree | Shared sticky token/cost accounting |
| Tool sidecars | Content-addressed artifacts for large results |
| Compaction | Preserve complete user/tool-batch boundaries and mechanical milestones |

Typed outbound copies are sanitized before the model trait call; the provider adapter independently checks its request copy after acquiring credentials. Native signed metadata is preserved or rejected as an atomic unit.

Exact model tokenizers, semantic compaction and broader long-horizon evaluation remain roadmap work. A budget estimate is not an invoice; uncertain provider attempts may need reconciliation.
