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
