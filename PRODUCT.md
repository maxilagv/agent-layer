# LayerAgent product specification

> Version 0.1.0 · development · MIT

LayerAgent is a local and self-hosted engineering agent. Its Rust runtime connects terminal, HTTP, web and desktop clients to persistent sessions, policy-controlled tools and explicitly configured model providers.

This document describes the intended product and the promises it makes.
[Implementation status](docs/10-implementation-status.md) describes supported
behavior, the evidence behind each promise and the gates that remain open. A
feature having code or a passing fixture is different from native platform
certification.

## Product principles

| Principle | Observable requirement |
| --- | --- |
| Operator control | Explicit project scopes, credential references, approvals and cancellation |
| Confined execution | Unsupported or unavailable confinement rejects execution |
| Durable work | Persist history, task identity and operation intent; reconcile uncertain actions before retry |
| Provider choice | Configured adapters and authorized fallbacks; optional local-model-only policy |
| Inspectable changes | File diffs, content hashes, tool outcomes and evidence provenance |
| Bounded learning | Stage, independently exercise and version reusable procedures |
| Shared runtime | CLI, TUI, web and desktop clients reuse the same core, protocol and persisted state |

## Current product boundary

The first version supports Linux only and targets a trusted operator on that host. Authenticated delegated principals receive explicit workspace grants. This is not a public, hostile multi-tenant execution service. macOS and Windows execution remain disabled and are outside this release. Shell and browser execution require a host that supports the documented Linux namespaces and confinement.

The web surface is local. It binds to a loopback address, is reached through a single-use code and serves one operator. Remote web access and multiple accounts are not part of this release.

## Promise matrix

Each promise names the test that checks it, the version or hash the result
belongs to, what was actually observed, what the promise does not cover and
what blocks delivery. A promise that was not executed is recorded as such; it
is never presented as kept.

| # | Promise | Scope | Test | Version / hash | Result | Omissions | Blocking condition |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | The same task is visible from every client | Task identity lives in the daemon, not in a connection; seven exposed states; cancellation recorded apart from the state | `cargo test --workspace`; `docs/10-implementation-status.md` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Only the local executor | A remote executor must be qualified |
| 2 | A repeated request never runs twice | `POST /v1/tasks` with an idempotency key; identical replay returns the same task; a reused key with other content is refused with 409 | `cargo test -p layeragent-storage --test tasks`; `cargo test -p layeragent-gateway --test tasks_http`; `tests/system_smoke.py` | the source fingerprint recorded in `docs/verification.json` for this revision | kept: CLI, TUI, web and desktop create the same durable task; the daemon smoke replays without a second turn | Remote executors and a desktop/web end-to-end run are not qualified | A remote executor and a clean-VM end-to-end run |
| 3 | A stale decision never applies | Approvals carry an expected revision, an argument hash and an expiry; stale or expired decisions are refused with 409 | `cargo test --workspace` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Browser-page semantics are untrusted | none |
| 4 | Artifacts stay with their work | Reads are authorized by project, session and task in addition to the principal | `cargo test --workspace` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | binary artifacts are not redacted | none |
| 5 | Corrections are applied where they are safe | A correction is persisted and applied at the next safe point of the engine, after the active tool resolves | `cargo test --workspace` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | A correction that arrives after the turn ends is rejected | none |
| 6 | Project files can be read as MCP resources | Paginated `resources/list` and `resources/read` through the same file broker and read grant, a 64 KiB UTF-8 byte cap per read with a pointer to the paginated file read, invalid cursors and prohibited paths refused; templates, prompts and notifications are not advertised | `cargo test -p layeragent-policy` | the source fingerprint recorded in `docs/verification.json` for this revision | kept: capability negotiation is repeated on reconnect, multi-byte text stays within the byte budget, and the catalog is paged | The catalog is the application-owned one and is bounded to 2000 entries | none |
| 7 | Installation is guided and resumable | `layeragent setup` detects the host, selects the provider and model, binds a credential, checks the connection, exercises the isolation for real and configures the browser; progress is saved atomically; a blocked capability offers a concrete retry | `cargo test -p layeragent-cli` (`setup_tests::*`); manual run | the source fingerprint recorded in `docs/verification.json` for this revision | kept for progress, selection, credential references and capability-scoped blocking | Clean-VM onboarding, a real system credential store, Chromium and a sandbox-failure drill were not exercised here | The target clean VM must exercise invalid-key, missing-dependency and sandbox-failure paths |
| 8 | Secrets stay out of the assistant state | Keys entered during setup live in the operating system credential store; installations without a store keep an environment reference | `cargo test -p layeragent-cli`; `layeragent_providers` keyring tests | the source fingerprint recorded in `docs/verification.json` for this revision | kept | A host without a credential store cannot use stored keys | none |
| 9 | Connections are visible and revocable | Providers, the restricted MCP adapter, runners and channels report an identifiable account, authorized projects, operations, state and revision without secrets; revocation raises the authority generation, stops pending tasks and refuses new effects | `cargo test -p layeragent-storage --test tasks`; `tests/system_smoke.py` | the source fingerprint recorded in `docs/verification.json` for this revision | kept: the smoke revokes a provider with a running task and receives 409 for subsequent work | Only local integrations are exercised | Live channel and provider qualification |
| 10 | The operator can work in a browser on their own machine | Loopback-only web surface with a single-use code, an HttpOnly session, CSRF protection, Host and Origin validation, a content security policy and server-side authorization before a task or an artifact is reached | `cargo test -p layeragent-web` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Remote access and multiple accounts | none |
| 11 | The same interface serves the desktop application | One React, TypeScript and Vite frontend with projects, conversations, tasks, executor, approvals with full context, diffs, artifacts, connections, usage and settings; keyboard navigation, visible focus, empty and error states, reconnection; each view loads a snapshot first and then ordered events; a project change discards the previous subscription and data | `npm run build`, `npm test` in `frontend/` with Node 22.23.4 | frontend tree as built (7 test files, 10 tests) | kept: the production build and the fixture tests pass | Verified against fixtures, not a daemon or the Tauri WebView | A clean-VM end-to-end run and a Tauri window |
| 12 | The desktop application adds no authority | Tauri shell for Linux that talks to the daemon over a UNIX socket with bounded commands, loads only bundled resources and has no general shell or file access; grants stay enforced by the daemon | desktop build record in `apps/desktop` | see `apps/desktop/README.md` | **not qualified**: the Tauri system packages cannot be installed in this environment without administrator rights, so the application was not compiled or run here | A compiled `.deb` and a running window | The build host must install the documented packages |
| 13 | The daemon keeps working when the window closes | Closing the window keeps the daemon and the tray alive; the tray shows the daemon and the active task count; quitting closes window and tray; stopping the daemon requests cancellation and shows the possible uncertain effects; autostart is optional and disabled by default | desktop build record | see `apps/desktop/README.md` | **not qualified** (same reason as 12) | Autostart was not exercised | Same as 12 |
| 14 | The browser never holds a remote secret | The daemon presents remote job metadata and its runner; the web and the WebView receive no pairing key and never contact the coordinator | `cargo test --workspace` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | The remote bridge is not deployed here | An independent deployment qualification |
| 15 | Performance statements follow the evidence | Queue, engine, tool, network and provider times, first useful content, total duration, p50 and p95, peak RAM, CPU, tokens and estimated cost are recorded with the hardware, versions, configuration, load, model and the source and binary hashes; two runs are only compared under the same conditions | `python3 tests/benchmark_tasks.py` | harness as committed | kept: a local fixture run recorded every stage, p50, peak RSS, CPU and tokens and labelled itself a fixture | No live provider sample was executed here: no private MiniMax credential is present in this environment | A live run with the private credential and a budget of USD 2 per run |
| 16 | A small budget still produces an honest number | Provider prices must be configured; the engine's input cap and a configured output cap bound each request; the projected cost of the next request is compared with the budget before it is sent; a supplied invoice amount is compared with the estimate; a sample that cannot reach a useful size publishes p95 as `not qualified` | `python3 tests/benchmark_tasks.py` | harness as committed | kept: a live run defaults to a USD 2 projected budget, refuses to start without prices or a bounded output cap, and compares `--billed-usd` when supplied | No billed amount exists without a live provider account | A live run with the private credential |
| 17 | Fixtures are not confused with the provider | Local fixture runs measure the engine and are labelled as such | `python3 tests/benchmark_tasks.py` | harness as committed | kept | none | none |

## Capability roadmap

| Area | Development implementation | Work before stable |
| --- | --- | --- |
| Runtime | Typed lifecycle, persistent sessions, cancellation, child-agent worktrees | Broader real-model task evaluations and restart testing |
| Execution | Linux namespace launcher, Landlock, seccomp, proxy, limits, PTY | Positive full native suite on supported hosts; platform certification |
| Interfaces | CLI/TUI, REST/SSE/WebSockets, MCP and channel adapters, loopback web surface, shared frontend, desktop shell | Desktop build, clean-VM end-to-end run, live channel qualification |
| Data | SQLite WAL, lexical and optional vector memory, encrypted offline backups | Operational drills, retention administration and load evidence |
| Integrations | Five wire adapters, custom tools, restricted project-files MCP with tools and resources, owner runtime MCP | Provider qualification and service-specific MCP adapters |
| Remote work | Encrypted personal/self-hosted bridge, mTLS runners, PostgreSQL leases and local effect receipts | Independent deployment qualification, load/retention operations and broader failure drills |
| Learning | Drafts, isolated fixtures, promotion, versions and rollback | Independent effectiveness measurements |
| Artifact analysis | Native metadata, evidence chain and external analyzer bridges | Installed-analyzer qualification and broader reconstruction |

## Acceptance criteria

A release is stable only when required tests pass against its recorded source fingerprint, native confinement succeeds on every advertised execution platform, failure recovery is demonstrated, and documented installation paths are exercised from a clean environment. Performance targets require reproducible measurements; no universal latency or superiority guarantee is made.

A failed mandatory test, a platform without a tested native isolation, or a failed clean installation blocks the stable label.

## Later product work

1. Qualify the desktop application on a host with the Tauri system packages and publish a `.deb` for the qualified platform.
2. Qualify deployment on an operator-owned server with TLS and backup drills.
3. Qualify the encrypted remote bridge across independently deployed hosts and expand its operational tooling.
4. Consider team accounts and SaaS after independently validating tenant isolation.

No calendar date substitutes for a passed acceptance criterion.
