# LayerAgent product specification

> Version 0.1.0 · development · MIT

LayerAgent is a local and self-hosted engineering agent. Its Rust runtime connects terminal, HTTP and messaging clients to persistent sessions, policy-controlled tools and explicitly configured model providers.

This document describes the intended product. [Implementation status](docs/10-implementation-status.md) describes supported behavior and evidence. A feature having code or a passing fixture is different from native platform certification.

## Product principles

| Principle | Observable requirement |
| --- | --- |
| Operator control | Explicit project scopes, credential references, approvals and cancellation |
| Confined execution | Unsupported or unavailable confinement rejects execution |
| Durable work | Persist history and operation intent; reconcile uncertain actions before retry |
| Provider choice | Configured adapters and authorized fallbacks; optional local-model-only policy |
| Inspectable changes | File diffs, content hashes, tool outcomes and evidence provenance |
| Bounded learning | Stage, independently exercise and version reusable procedures |
| Shared runtime | Clients reuse the same core and protocol |

## Current product boundary

The first version supports Linux only and targets a trusted operator on that host. Authenticated delegated principals receive explicit workspace grants. This is not a public, hostile multi-tenant execution service. macOS and Windows execution remain disabled and are outside this release. Shell and browser execution require a host that supports the documented Linux namespaces and confinement.

## Capability roadmap

| Area | Development implementation | Work before stable |
| --- | --- | --- |
| Runtime | Typed lifecycle, persistent sessions, cancellation, child-agent worktrees | Broader real-model task evaluations and restart testing |
| Execution | Linux namespace launcher, Landlock, seccomp, proxy, limits, PTY | Positive full native suite on supported hosts; platform certification |
| Interfaces | CLI/TUI, REST/SSE/WebSockets, MCP and channel adapters | Desktop/web product interfaces, live channel qualification |
| Data | SQLite WAL, lexical and optional vector memory, encrypted offline backups | Operational drills, retention administration and load evidence |
| Integrations | Five wire adapters, custom tools, MCP client/server | Provider qualification, richer MCP administration and authentication |
| Learning | Drafts, isolated fixtures, promotion, versions and rollback | Independent effectiveness measurements |
| Artifact analysis | Native metadata, evidence chain and external analyzer bridges | Installed-analyzer qualification and broader reconstruction |

## Acceptance criteria

A release is stable only when required tests pass against its recorded source fingerprint, native confinement succeeds on every advertised execution platform, failure recovery is demonstrated, and documented installation paths are exercised from a clean environment. Performance targets require reproducible measurements; no universal latency or superiority guarantee is made.

## Later product work

1. Complete local onboarding, project management and action review.
2. Qualify deployment on an operator-owned server with TLS and backup drills.
3. Add web and desktop clients on the existing protocol.
4. Introduce authenticated remote runners and durable distributed scheduling.
5. Consider team accounts and SaaS after independently validating tenant isolation.

No calendar date substitutes for a passed acceptance criterion.
