# 10 · Promise matrix, verification and release gates

> LayerAgent 0.1.0 · Linux development release. Source presence, fixture success and host qualification are separate states.

## How to read this document

Every row is a promise made to an operator. A promise is only *kept* when the
named test exists, was executed against the recorded source fingerprint and
produced the recorded result. Rows that could not be executed in the current
environment say so; they are not counted as passes.

Columns:

| Column | Meaning |
| --- | --- |
| Scope | What the promise covers |
| Test | The command or file that checks it |
| Version / hash | The source or binary the result belongs to |
| Result | What actually happened |
| Omissions | What the promise does not cover |
| Blocking condition | What must happen before delivery |

## Promise matrix

| # | Scope | Test | Version / hash | Result | Omissions | Blocking condition |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Durable task identity independent of the connection that created it, with the seven exposed states and a separate cancellation state | `cargo test --workspace` (`layeragent_storage` task tests, `layeragent_gateway` task routes) | the source fingerprint recorded in `docs/verification.json` for this revision | kept: the task record, the event log, the corrections and the cancellation states are persisted and survive a restart | Remote executors other than `local` are refused with `executor is not available` | A remote executor must pass its own qualification |
| 2 | `POST /v1/tasks` with an idempotency key, cursor queries, cancel and correct; replay returns the same task, a reused key with other content returns 409 | `cargo test -p layeragent-storage --test tasks`; `cargo test -p layeragent-gateway --test tasks_http`; `tests/system_smoke.py` | the source fingerprint recorded in `docs/verification.json` for this revision | kept: every client (CLI, TUI, web and desktop) creates durable tasks; the daemon smoke replays a request without starting a second turn | Remote executors are refused; desktop/web end-to-end execution is not qualified | A remote executor and a clean-VM end-to-end run |
| 3 | Approvals resolved with an expected revision, an argument hash and an expiry; stale clients receive 409 | `cargo test --workspace` (approval conflict tests) | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Approval expiry is enforced by the engine timer, not by a wall-clock sweep | None for the local engine |
| 4 | Artifact reads authorized by project, session and task in addition to the principal | `cargo test --workspace` (`artifact_scoped` tests) | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Binary artifacts are still byte exact and are not redacted | None |
| 5 | MCP resources for project files on the restricted adapter: paginated `resources/list`, `resources/read` through the same file broker and read grant, a 64 KiB UTF-8 byte cap per read, larger files point to the paginated file read, invalid cursors and prohibited paths refused | `cargo test -p layeragent-policy` (`resources_are_listed_and_read_through_the_same_grant`, `resources_are_paginated_and_an_invalid_cursor_is_refused`, `resource_reads_are_bounded_to_64_kib`, `multi_byte_resources_stay_within_the_byte_budget`, `prohibited_resource_uris_are_refused`, `resource_capability_is_renegotiated_on_reconnect`, `a_project_without_a_read_grant_serves_no_resources`) | the source fingerprint recorded in `docs/verification.json` for this revision | kept: each connection negotiates the capability again, the catalog is paged, and outside/symlinked paths are refused | Resource templates, prompts and notifications are not advertised; the catalog is bounded to 2000 entries | Nothing for this milestone |
| 6 | `layeragent setup`: resumable wizard, atomic progress, system credential store, real isolation test, blocked capability with a concrete retry | `cargo test -p layeragent-cli` (`setup_tests::*`) | the source fingerprint recorded in `docs/verification.json` for this revision | kept for progress, provider/model selection, unset credential references and capability-scoped blocking; the config writer serializes the whole document atomically | A clean Ubuntu VM run, a real system credential store, Chromium and a sandbox-failure drill were not available here | Clean-VM onboarding (invalid key, missing dependency, sandbox failure) and Chromium must be exercised on the target host |
| 7 | Connection service for providers, the restricted MCP adapter, runners and channels: identifiable account, authorized projects, operations, state and revision, no secrets; revoke raises the authority generation, stops pending tasks and refuses new work | `cargo test -p layeragent-storage --test tasks` (`revocation_advances_the_authority_generation`); `tests/system_smoke.py` (`connection_revoke_stops_pending`) | the source fingerprint recorded in `docs/verification.json` for this revision | kept: the daemon smoke revokes the provider while a task is running, observes a terminal task and receives 409 for the next submission | Only the local provider and MCP adapters are exercised | Live channel and provider qualification remain |
| 8 | Shared frontend with React, TypeScript and Vite: projects, conversations, tasks, executor, approvals with full context, diffs, artifacts, connections, usage and settings; keyboard navigation, visible focus, empty and error states, reconnection; snapshot first and then ordered events; a project change discards the previous subscription and data | `npm run build` and `npm test` in `frontend/` with Node 22.23.4 | frontend tree as built (7 test files, 10 tests) | kept: the production build succeeds and the project-switch, ordering, reload, approval and stale-correction tests pass | The frontend is verified against fixtures, not against a qualified daemon run or the Tauri WebView | The desktop and clean-VM end-to-end run is still required |
| 9 | Loopback web surface with a one-time code, an HttpOnly session, CSRF protection, Host and Origin validation, a content security policy and server-side authorization | `cargo test -p layeragent-web` | the source fingerprint recorded in `docs/verification.json` for this revision | kept | Remote access and multiple accounts are out of scope for this milestone | None |
| 10 | Desktop application packaged with Tauri for Linux, talking to the daemon over a UNIX socket with bounded commands and minimal permissions | `sh apps/desktop/verification/run.sh`; `cargo fmt --check` and `cargo metadata` in `apps/desktop/src-tauri` | desktop tree as built | **not qualified**: the Tauri system packages (`libwebkit2gtk-4.1-dev` and friends) cannot be installed in this environment without administrator rights, so the crate was not compiled here. The bridge sources are type-checked and unit tested by the harnesses in `apps/desktop/verification/`. | A compiled `.deb` and a real window were not produced | The build host must install the documented packages and the crate must compile and run |
| 11 | Closing the window leaves the daemon working; the tray shows the daemon and the active task count; quitting closes window and tray; stopping the daemon requests cancellation and surfaces uncertain effects | desktop build record | see `apps/desktop/README.md` | **not qualified** (same reason as 10) | Autostart is implemented as an option and is disabled by default | Same as 10 |
| 12 | The daemon presents remote job metadata and its runner to the frontends; the web and WebView never receive pairing keys and never contact the coordinator | `cargo test --workspace` (remote protocol tests) | the source fingerprint recorded in `docs/verification.json` for this revision | kept | The remote bridge itself is not deployed in this milestone | An independent deployment qualification is required before it is advertised |
| 13 | Instrumented queue, engine, tool, network and provider times, first useful content, total duration, p50 and p95, peak RAM, CPU, tokens and estimated cost | `python3 tests/benchmark_tasks.py` (local fixture run executed; live run gated) | harness as committed | kept: the harness records conditions and hashes, requires configured prices and a bounded output cap for a live run, defaults the projected budget to USD 2 per run, compares an operator-supplied billed amount and publishes `p95: "not qualified"` for a sample that is too small | No live provider sample was executed here; no MiniMax credential is present in this environment; the fixture sample covers two `small_project` runs and is labelled `local fixture` | A live MiniMax run with the private credential and the USD 2 projection |
| 14 | Reproducible cases: small project, large repository, four sessions, long work and search with cold and warm cache | `python3 tests/benchmark_tasks.py --cases ...` | harness as committed | kept: all five cases are implemented and runnable; the executed fixture run covered `small_project` and completed twice | Four sessions run as separate tasks, not one orchestrated concurrent scenario; a full five-case qualified run was not executed here | A qualified provider run with a real workload |
| 15 | Secure, memory and performance statements only where evidence exists | this document and `PRODUCT.md` | the source fingerprint recorded in `docs/verification.json` for this revision | kept: every claim names its test and its limits | No universal latency, memory or superiority claim is made | None |

## Required scenario coverage

| Scenario | Executable evidence | Result |
| --- | --- | --- |
| Lost response and replay | `tasks::identical_replay_returns_the_same_task_and_reuse_conflicts`; `tasks_http::create_replay_and_conflict_have_distinct_status_codes`; `tests/system_smoke.py` | pass: one task, 201 then 200, and 409 on key reuse with other content |
| Cancel during a response | `tasks_runtime::cancel_during_response_is_confirmed_and_terminal` | pass: `cancelled` with `confirmed` cancellation |
| Correction during an active tool | `tasks_runtime::correction_waits_for_the_active_tool_and_is_applied_once` | pass: applied only at the next safe point |
| Daemon restart | `tasks::restart_blocks_live_tasks_and_never_resumes_them`; `tests/system_smoke.py` (SIGKILL) | pass: `blocked`, `reconcile` error, no resumed turn |
| Two tabs approving | `tasks_runtime::stale_or_second_approval_is_rejected`; `tasks_http::approval_without_the_observed_revision_is_a_conflict` | pass: the second/stale decision is refused |
| Project switch | `frontend/src/__tests__/project-switch.test.tsx` | pass: previous data and subscription discarded |
| Foreign artifact download | `tasks::artifacts_are_scoped_to_the_task_and_session_that_produced_them`; `tasks_http::task_read_correction_and_artifact_scopes_return_typed_errors` | pass: foreign principal/task receives a typed refusal |
| Revoke with pending tasks | `tests/system_smoke.py` (`connection_revoke_stops_pending`); `tasks::revocation_advances_the_authority_generation` | pass: pending work becomes terminal and new work is refused with 409 |
| MCP missing capability | `mcp::a_project_without_a_read_grant_serves_no_resources` | pass: no capability advertised, list/read refused |
| MCP capability change on reconnect | `mcp::resource_capability_is_renegotiated_on_reconnect`; `files::changing_a_reviewed_mcp_grant_revokes_existing_broker_authority` | pass: a new connection negotiates again; the old authority is revoked |
| Invalid resource cursor | `mcp::resources_are_paginated_and_an_invalid_cursor_is_refused` | pass: paging works and an unparseable cursor is an error |
| Large and prohibited resource | `mcp::resource_reads_are_bounded_to_64_kib`; `mcp::multi_byte_resources_stay_within_the_byte_budget`; `mcp::prohibited_resource_uris_are_refused` | pass: a 64 KiB byte budget with a pagination pointer, including multi-byte text; symlink/traversal refused |
| Grant change | `files::changing_a_reviewed_mcp_grant_revokes_existing_broker_authority` | pass: the reviewed grant digest is checked before use |
| Clean Ubuntu VM onboarding (invalid key, missing dependency, sandbox failure) | not executable here | **blocked**: no clean VM or administrator rights in this environment; the setup unit tests cover progress, credential-reference and capability-scoped blocking only |
| Same task across CLI/TUI | `tests/system_smoke.py` | pass: the CLI creates and follows the durable task, cancellation uses the task route |
| Desktop/web reload and reconnect without duplicate execution | `frontend/src/__tests__/reload.test.tsx`; `ordering.test.ts`; `stale-correction.test.tsx`; `layeragent-web` tests | partial: reload/new tab reads the durable snapshot again without creating a task, and reconnect is tested against fixtures; no Tauri window or desktop end-to-end run exists yet |

The workspace suite for this revision passed 202 tests with 10 ignored, plus
the system smoke and the frontend build/tests. The ignored tests require
native namespaces, live providers or an independent deployment; they are not
counted as passes.

## Capability matrix

| Subsystem | Implemented | Verification boundary / remaining work |
| --- | --- | --- |
| Linux execution | Mount roots, protected metadata, Landlock, seccomp, process groups, limits, proxy and optional cgroups | Explicit native suite checks real namespaces, cgroups, filesystem/network denial and positive controls. Run in a delegated service; exact snapshot outcomes belong in generated evidence. |
| Browser | Outer Linux confinement, Chromium sandbox, private profile and controlled proxy | Real Chromium navigation, forms, screenshots, network decisions and search extraction have been exercised. Search extraction does not establish result relevance. No unsafe bypass or unconfined attachment. |
| macOS/Windows | Experimental backend source and platform-specific policy generation | Outside the Linux-only first release; production execution disabled. |
| Lifecycle | Typed edges, durable intent/results, uncertainty recovery, scoped cancellation | Scripted lifecycle tests plus executable restart/transport smokes; real external-effect fault drills remain required. |
| Tasks | Durable task identity, idempotent submission, corrections at the next safe point, cancellation states, scoped artifacts, revocation stopping pending work | Local executor only. Remote executors are refused until they are qualified. |
| Subagents | Shared budgets, concurrency, cascading stop and isolated worktrees | Scripted models; broader real-model effectiveness remains unqualified. |
| Gateway | Authenticated API/streams, owner-only administration, grants and bounded admission | Authorization/transport fixtures and real executable smoke tests; not hostile multi-tenant SaaS certification. |
| Web surface | Loopback-only binding, one-time code, HttpOnly session, CSRF, Host and Origin validation, CSP, server-side authorization | Local operator only. Remote access and multiple accounts are out of scope. |
| Frontend | Shared React, TypeScript and Vite application for web and desktop | Node 22 build and 10 fixture tests pass; a clean-VM end-to-end run is still required. |
| Desktop | Tauri shell over the daemon socket with bounded commands | Bridge sources pass the stub type-check and 9 unit tests; the Tauri packages are not installed here, so no window or `.deb` was built. |
| MCP | Application-owned project-files service with tools and resources; exact grants, reviewed catalog and descriptor/Landlock enforcement | Arbitrary HTTP/stdio integrations fail closed pending verified adapters. Resource templates, prompts and notifications are not advertised. |
| Remote execution | mTLS pairing, signed age-encrypted jobs, PostgreSQL assignments, lease/generation fencing, local receipts, CLI/TUI, scratch agent execution and checkpoint recovery | Disposable-PostgreSQL/TLS smoke and delegated Linux native tests are explicit qualification steps. Distinct-UID production deployment, independent-host partitions and sustained load remain deployment gates. |
| Providers | Five adapters, fallback, OAuth/keyring, privacy policy and credential registration | MiniMax through Vercel AI Gateway has a real opt-in streaming/restart/tool check. Other provider routes and OAuth still need their own live qualification before being advertised as verified. |
| Plaintext handling | Typed/native copy sanitization, bounded stream redaction, sanitized durable text | Supported patterns and registered values only; binary/OCR/encoded secrets and historical exports are not covered. |
| Memory | WAL/FULL, scoped BM25/cache, optional embedding worker, HNSW/rank fusion, fact revisions | Optional build and deterministic retrieval tests; actual embedding inference and quality need qualification. |
| Skills | Drafts, credential rejection, confined fixtures, promotion/version rollback | Working native confinement required for real promotion; general synthesis quality unproven. |
| Analysis | Metadata, evidence chain/checkpoints and analyzer bridges | External analyzers need installation/qualification; no general reconstruction claim. |
| Backup | Authenticated encrypted offline snapshots, manifest verification, new-home restore and retention | Local roundtrip, wrong-key, tamper and busy-home tests; recurring operational drills remain necessary. |
| Setup | Resumable wizard with atomic progress, system credential store and a real isolation test | Browser and provider steps need a configured provider and Chromium; clean-VM onboarding is not yet executed. |
| Measurement | Task harness with conditions, budget guard, billed-cost comparison and qualified percentile rules | A live provider sample with the private credential is still required. |
| Source distribution | Standalone docs, allowlisted source bundle, dependency inventory, detached-signature tooling and hygiene tests | Disposable-key signing tests only; no trusted stable-release signing key or prebuilt release certification. |

## Reproducible verification

```bash
python3 tests/verify.py
sudo -v
python3 tests/verify.py --native
# On hosts with an already delegated user manager:
python3 tests/verify.py --native-user
python3 tests/remote_smoke.py --native --user-service
# Task measurement (labels a run without configured prices as a fixture):
python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 5
# Live measurement: prices must be configured; the projected budget defaults to USD 2.
python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 20 \
    --budget-usd 2 --billed-usd <invoice-amount> --output docs/task-benchmark.json
# Shared frontend (Node 22) and the desktop bridge harnesses:
(cd frontend && npm run build && npm test)
sh apps/desktop/verification/run.sh
```

`verification.json` is generated evidence: it records the exact maintained source fingerprint, command outcomes, ignored tests and whether inputs changed during verification. A previous report does not certify edited source. Generated reports are excluded from the fingerprint to avoid self-reference. Tests using fixture models do not make paid requests.

Bounded seeded stress/property tests cover fragmented redaction, atomic signed native blocks and malformed/mixed-delimiter provider frames. They are not a coverage-guided fuzzing campaign. Source-signing tests require OpenSSL 3 and disposable keys; recipients still need an independently authenticated release public key.

Native status is recorded independently. An ignored test is not a pass. This environment initially blocked namespace creation; the operator installed the scoped AppArmor profiles, and qualification uses a transient delegated systemd service. The system-manager variant needs administrator authentication. An explicitly selected, already delegated systemd user manager can run the same assertions without changing host security. Both require systemd 254+ and the documented kernel features. Qualification covers the tested host configuration, not every Linux installation. macOS and Windows are outside the first-release scope.

Browser live tests require the CLI helper path and may download Chromium. Provider live tests are opt-in and consume real usage; they load a private external credential file and can emit a private report containing source/binary fingerprints. Fingerprints alone do not prove binary-to-source linkage. Run the documented build immediately before qualification and inspect change flags. Model reasoning and credential values are suppressed.

## P0 acceptance ledger

| P0 area | Change / acceptance evidence | Remaining gate |
| --- | --- | --- |
| Single status source | This matrix and standalone product spec | Keep synchronized with maintained source |
| Current verification | Fingerprinted repeatable verifier and executable smoke tests | Inspect generated report for final run |
| Linux sandbox | Native helpers and explicit full namespace suite | Exact source/host qualification must pass |
| Resource ceilings | Finite CPU/file/open-file defaults; native cgroup/process checks | Requested controllers and enforcement checked per workload |
| Windows/macOS | Fail-closed production execution | Outside this Linux-only P0 |
| Browser | Outer confinement, real Chromium checks and proxy-traversal assertions | Host support and result-quality limits remain explicit |
| Secrets | Credential registration, typed storage/provider copies and split-stream tests | Pattern limits explicitly documented |
| Action permissions | Destination/argument/context approvals and revocation | Dynamic web semantics remain untrusted |
| Information egress | Provider allowlist/local-only policy; proxy allowlists and approvals | Not a universal information-flow proof |
| API | Owner boundary, ambiguous credential rejection, admission limits | Public deployment qualification |
| Task identity | Durable tasks, idempotency, corrections, cancellation states, scoped artifacts | Remote executor qualification |
| Web and desktop | Loopback surface with a one-time code, shared frontend, Tauri shell | Desktop build and clean-VM end-to-end run |
| Recovery | Durable uncertain outcomes, repeatable recovery tests | Real-provider/channel fault drills |
| Backup/restore | Authenticated snapshots and tamper/restore tests | Operator schedules and restoration drills |
| Supply chain | Locked dependencies, dependency policy and local bundle hygiene | Advisory output and platform evidence must be reviewed |
| Product claims | Explicit scope, limits and acceptance gates | Stable release stays gated |

## Historical measurements

Existing benchmark JSON files are historical synthetic measurements. They must not be presented as current-binary results without a new source/binary pairing. No universal 2 ms memory target, sub-10 ms confinement claim or comparative superiority is certified.
