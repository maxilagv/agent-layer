# 11 · Configuration and operations

Configuration merges defaults, `HOME/config.toml`, then `HOME/profiles/NAME.toml`. Choose a profile with `--profile NAME`. Invalid reloads retain the last valid snapshot. Changes to execution authority cancel work and pending approvals.

```bash
layeragent config validate
layeragent config explain
layeragent config schema
layeragent models
layeragent doctor --json
```

Use [examples/config.toml](../examples/config.toml) as an annotated reference. Keep runtime home outside workspaces and credential values outside the configuration file.

## Useful policy controls

| Setting | Meaning |
| --- | --- |
| `policy.shell` / `write` | Enable shell and workspace writes |
| `policy.allowed_tools` | Optional tool allowlist |
| `policy.approval_tools` | Additional tools requiring approval |
| `policy.read_roots` / `write_roots` | Additional absolute scoped roots |
| `policy.protected_paths` | Metadata paths kept protected |
| `policy.network` + `allow_domains` | Confined shell network policy |
| `policy.limits` | Resource ceilings; delegated cgroups are opt-in and fail closed |
| `privacy.local_models_only` | Forbid remote model endpoints |
| `privacy.allowed_providers` | Restrict eligible providers and fallbacks |
| `browser.enabled` / `allow_domains` | Separate browser network policy |

Default process ceilings include 120 CPU seconds, a 256 MiB individual-file limit and 256 open files. Memory/pid/cgroup quotas need workload-specific configuration and a supporting host. The service template additionally provides aggregate limits.

`doctor` validates the selected configuration and reports `required` for each JSON check. Namespaces, Landlock and Python are required. Node is optional; cgroup delegation is required only when `policy.limits.cgroup = true`. An unavailable optional check prints `WARN` and does not block the command. Requesting unavailable cgroups still fails closed, and effective enforcement is checked again during execution.

For full native qualification, install the scoped AppArmor profiles when needed, authenticate with `sudo -v` in your terminal and run `python3 tests/verify.py --native`. The runner creates a transient delegated systemd service for the tests; it requires systemd 254+ and does not install a persistent daemon. A successful run qualifies that host and source snapshot, not every Linux distribution.

## HTTP service

Set a strong owner token in the referenced environment variable, then:

```bash
layeragent serve
```

The default listener is loopback. For remote access, use TLS termination, correctly forwarded host/origin information, a private service environment and scoped delegated tokens. Review `deploy/layeragent.service` before installing it as a user service. Do not expose owner MCP to callers who should have only delegated workspace access.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| Namespace/loopback permission denied | `doctor`, host kernel and AppArmor profile |
| HTTP 401 | Owner/delegated token reference and actual environment |
| HTTP 403 | Owner-only operation, Origin mismatch or read-only identity |
| HTTP 429/503 | Rate, connection or job admission limits |
| Approval expired | Configuration change, timeout or changed browser target |
| Memory degraded | Embedding build, ONNX Runtime and model cache |
| Backup busy | Stop every profile using the home before taking a snapshot |
| Unknown operation/delivery | Reconcile the external effect before repeating |

Never resolve a failed security preflight by enabling an unconfined fallback.
