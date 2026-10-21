# Security policy

LayerAgent 0.1.0 is a development build. It is intended for a trusted operator and explicitly authorized principals on a self-hosted machine. It has not been certified as a public multi-tenant service.

## Reporting

The public repository is [maxilagv/agent-layer](https://github.com/maxilagv/agent-layer). A dedicated private reporting channel has not been established. Report security issues privately to the maintainer through an existing private contact; do not disclose sensitive findings in public issues. Do not attach real credentials, private customer data or weaponized public demonstrations. Include the affected version/source fingerprint, prerequisites, reproduction and observed impact. No response-time commitment is currently offered.

## Boundaries and controls

| Boundary | Enforcement | Qualification |
| --- | --- | --- |
| Tool filesystem | Linux namespaces, runtime mounts, protected paths and Landlock | Full native execution requires a supported host |
| Tool network | Explicit allowlist proxy; private destinations rejected | Test proxy and complete namespace path separately |
| Browser | Outer Linux namespaces plus Chromium's sandbox and controlled proxy | No production `--no-sandbox`, host attachment or visible-mode fallback |
| Global administration | Owner identity, independent of delegated scope strings | Delegated callers cannot reload global config or use owner MCP |
| Delegated work | Tenant/principal and workspace grants | Not a substitute for hostile-tenant compute isolation |
| External operations | Concrete action approval, configured destination, argument digest | Page scripts remain untrusted; approvals cannot prove page intent |
| Credential handling | Environment/keyring references, exact registration and plaintext redaction | Heuristic detection is not a universal secret classifier |
| History and replay | Sanitized durable copies and original-argument fingerprints | Live tool arguments remain exact; old data needs operator review |
| Signed model metadata | Preserve clean atomic units; reject detected sensitive signed units | Never rewrite signed thinking in place |
| Backups | Authenticated age encryption, manifests, offline home lock | Keep the private identity separately; exercise restoration |

## Explicit limits

- Local administrators and processes with the same account can control the host. This is not an administrator-resistant system.
- Kernel, browser and dependency vulnerabilities remain relevant. Sandboxing reduces exposure; it does not prove absence of vulnerabilities.
- Redaction handles registered exact values and supported patterns. It does not detect every arbitrary secret, steganography, image text, encoded data or intentional exfiltration. Binary artifacts remain byte-exact.
- Streaming redaction buffers incomplete logical lines, suppresses oversized records and tracks private-key bodies. Short text without a newline may appear only at completion.
- A local-model-only policy constrains model destinations; browser, tool and messaging policies must also be disabled or restricted for offline operation.
- Updating does not retroactively cleanse old databases, artifacts, exports or backups. Rotate any exposed credentials and review historical data.
- Native macOS/Windows execution is disabled pending certification. Compilation is not certification.
- An evidence hash chain detects changes relative to a trusted checkpoint; an administrator can replace local files and keys.

## Safe operation

Keep runtime state outside workspaces. Prefer a dedicated account for remote operation, narrow workspace grants, explicit domain allowlists and shell approvals. Bind HTTP to loopback and terminate TLS through a correctly configured proxy when remote access is needed. Keep the service environment private and use separate tokens for delegated identities.

## Release gate

A release must not claim full P0 closure while native execution, platform certification or a relevant security check remains blocked. See [implementation status](docs/10-implementation-status.md) and generated verification evidence for exact outcomes.
