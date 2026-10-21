# 10 · Implementation status and release gates

> LayerAgent 0.1.0 · Linux development release. Source presence, fixture success and host qualification are separate states.

## Capability matrix

| Subsystem | Implemented | Verification boundary / remaining work |
| --- | --- | --- |
| Linux execution | Mount roots, protected metadata, Landlock, seccomp, process groups, limits, proxy and optional cgroups | Explicit native suite checks real namespaces, cgroups, filesystem/network denial and positive controls. Run in a delegated service; exact snapshot outcomes belong in generated evidence. |
| Browser | Outer Linux confinement, Chromium sandbox, private profile and controlled proxy | Real Chromium navigation, forms, screenshots, network decisions and search extraction have been exercised. Search extraction does not establish result relevance. No unsafe bypass or unconfined attachment. |
| macOS/Windows | Experimental backend source and platform-specific policy generation | Outside the Linux-only first release; production execution disabled. |
| Lifecycle | Typed edges, durable intent/results, uncertainty recovery, scoped cancellation | Scripted lifecycle tests plus executable restart/transport smokes; real external-effect fault drills remain required. |
| Subagents | Shared budgets, concurrency, cascading stop and isolated worktrees | Scripted models; broader real-model effectiveness remains unqualified. |
| Gateway | Authenticated API/streams, owner-only administration, grants and bounded admission | Authorization/transport fixtures and real executable smoke tests; not hostile multi-tenant SaaS certification. |
| MCP | Application-owned project-files service; exact grants, reviewed catalog and descriptor/Landlock enforcement; two owner runtime server tools | Arbitrary HTTP/stdio integrations fail closed pending verified adapters. MCP transport and real rejected file effects have dedicated tests. |
| Remote execution | mTLS pairing, signed age-encrypted jobs, PostgreSQL assignments, lease/generation fencing, local receipts, CLI/TUI, scratch agent execution and checkpoint recovery | Disposable-PostgreSQL/TLS smoke and delegated Linux native tests are explicit qualification steps. Distinct-UID production deployment, independent-host partitions and sustained load remain deployment gates. |
| Providers | Five adapters, fallback, OAuth/keyring, privacy policy and credential registration | MiniMax through Vercel AI Gateway has a real opt-in streaming/restart/tool check. Other provider routes and OAuth still need their own live qualification before being advertised as verified. |
| Plaintext handling | Typed/native copy sanitization, bounded stream redaction, sanitized durable text | Supported patterns and registered values only; binary/OCR/encoded secrets and historical exports are not covered. |
| Memory | WAL/FULL, scoped BM25/cache, optional embedding worker, HNSW/rank fusion, fact revisions | Optional build and deterministic retrieval tests; actual embedding inference and quality need qualification. |
| Skills | Drafts, credential rejection, confined fixtures, promotion/version rollback | Working native confinement required for real promotion; general synthesis quality unproven. |
| Analysis | Metadata, evidence chain/checkpoints and analyzer bridges | External analyzers need installation/qualification; no general reconstruction claim. |
| Backup | Authenticated encrypted offline snapshots, manifest verification, new-home restore and retention | Local roundtrip, wrong-key, tamper and busy-home tests; recurring operational drills remain necessary. |
| Source distribution | Standalone docs, allowlisted source bundle, dependency inventory, detached-signature tooling and hygiene tests | Disposable-key signing tests only; no trusted stable-release signing key or prebuilt release certification. |

## Reproducible verification

```bash
python3 tests/verify.py
sudo -v
python3 tests/verify.py --native
# On hosts with an already delegated user manager:
python3 tests/verify.py --native-user
python3 tests/remote_smoke.py --native --user-service
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
| Recovery | Durable uncertain outcomes, repeatable recovery tests | Real-provider/channel fault drills |
| Backup/restore | Authenticated snapshots and tamper/restore tests | Operator schedules and restoration drills |
| Supply chain | Locked dependencies, dependency policy and local bundle hygiene | Advisory output and platform evidence must be reviewed |
| Product claims | Explicit scope, limits and acceptance gates | Stable release stays gated |

## Historical measurements

Existing benchmark JSON files are historical synthetic measurements. They must not be presented as current-binary results without a new source/binary pairing. No universal 2 ms memory target, sub-10 ms confinement claim or comparative superiority is certified.
