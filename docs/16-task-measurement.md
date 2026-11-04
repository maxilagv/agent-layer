# 16 · Task measurement

> LayerAgent 0.1.0 · Linux development release. Measurements are observations, not product claims.

## What is measured

The runtime records, per task:

| Value | Meaning |
| --- | --- |
| `queue_ms` | Time between the task record and the start of the turn |
| `engine_ms` | Time in the engine that is neither provider nor tool time |
| `tool_ms` | Time inside tool execution |
| `provider_ms` | Time waiting for the model provider |
| `network_ms` | Time in network transfer |
| `first_content_ms` | Time until the first useful content is visible |
| `total_ms` | Total duration of the task |
| `max_rss_bytes` | Peak resident memory of the daemon process |
| `cpu_ms` | CPU time of the daemon process |
| `input_tokens`, `output_tokens` | Token counters of the provider |
| `cost_usd` | Estimated cost from the configured prices |

## The harness

```bash
python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 5 \
  --output docs/task-benchmark.json
```

The harness talks to the running daemon over the profile socket and runs the
reproducible cases:

| Case | Workload |
| --- | --- |
| `small_project` | A workspace with a few files |
| `large_repository` | A workspace with 400 generated modules |
| `four_sessions` | Four concurrent sessions |
| `long_work` | A workspace with many files to read |
| `search_cache` | A search with a cold and a warm cache |

Each case is measured with several samples. The report contains the p50 and
the p95, the peak memory, the CPU time, the token counters and the estimated
cost of every sample.

## Conditions are part of the result

The report records the conditions of the run:

- hardware: platform, CPU model, logical CPUs, total memory, load average;
- versions: `rustc`, `cargo`, `node`;
- the configuration revision of the profile;
- the selected model and the provider base URL;
- the source fingerprint and the hash of the binary that produced the numbers.

Two reports are only compared when the hardware and the model match:

```bash
python3 tests/benchmark_tasks.py --compare before.json after.json
```

When they do not match, the comparison is reported as not qualified with the
reason.

## The cost rules

A live measurement requires configured prices. If the selected model has no
`input_per_million` or `output_per_million`, the harness refuses to run.

Before every request the harness computes the projected cost of that request
from the configured prices and the configured output token cap. When the next
request would exceed the budget, the run stops before sending it:

```bash
python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 20 \
  --budget-usd 2 --model minimax --output docs/task-benchmark.json
```

The report contains the estimated cost and, when the provider account exposes
one, the billed amount. An estimate is never presented as a bill.

## Qualified percentiles

A p95 computed from a handful of samples is noise. The harness therefore
publishes `p95_ms` as `"not qualified"` unless at least twenty samples were
measured, and records the reason:

```json
{
  "p95_ms": "not qualified",
  "p95_reason": ["fewer than 20 samples were measured"]
}
```

The same rule applies when the budget stops the run early.

## Fixtures are not provider latency

A local model measures the engine. Those runs are labelled
`"measurement": "local fixture"` and their provider times are not comparable
with a remote provider. The report always states which kind of measurement it
contains.

## Live provider sample

A qualified live sample needs the private provider credential, configured
prices and a budget. The credential is never written into the report: only
the model, the prices and the resulting costs are recorded. The MiniMax route
through the Vercel AI Gateway is exercised by `tests/live_provider.py`, which
is opt-in and consumes real usage.

## Spending controls

A task can carry its own budget, which covers every child agent it starts
and, for a team, becomes the team's cost ceiling unless that is lower:

```bash
layeragent run "Build the landing page" --workspace . --budget-usd 3
layeragent cost TASK_ID          # what it spent, per model and agent level
layeragent cost TASK_ID --json
```

In the interactive terminal, `/budget 3` applies to the next prompts and
`/budget off` removes it. A budget needs input and output prices for the
selected model. It can only narrow `agents.tree_max_cost_usd`, never widen it.

- **Before each request** the provider reserve may not exceed what is left of
  the budget, so the request that would overspend is refused.
- **While it works** the turn that owns the task is told when the task
  crosses 50%, 80% and 95% of its budget, once each; child agents spend from
  the same budget but are not told, and see it with `budget_status`. The token ceiling of the agent tree counts too,
  so a task without a money budget is still told as it nears
  `agents.tree_max_tokens`.
- **`budget_status`** shows the turn's tokens, cached share and cost, the
  task's spend against its budget and token ceiling, the team's usage for a
  member, and the configured models it may delegate to, cheapest first, with
  their prices and notes.
- **`spawn_agent` with `model`** gives a child a cheaper model. The operator's
  rules hold: a model must be allowed by the privacy settings, may not be
  `team_access = "none"`, must be `"write"` for a writable child, and must be
  priced when the task has a cost limit.

Every provider call is recorded in a usage ledger with its model, agent
depth, tokens (cached ones apart) and estimated cost.
`GET /v1/tasks/{id}/usage` and `layeragent cost` add it up per model and per
depth, including every member activation of a team, and list hints such as
enabling `prompt_cache` for an uncached Anthropic model, delegating many short
calls of an expensive model to a cheaper one, or narrowing children that spent
most of the task. Hints are suggestions; nothing is changed automatically.
