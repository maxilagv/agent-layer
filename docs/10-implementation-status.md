# 10 · Promise matrix, verification and release gates

> LayerAgent 0.1.0 · Linux development release. Source presence, fixture success and host qualification are separate states.

## Cooperative terminal execution

`execution: cooperative` exposes teams through CLI/TUI and the task API. Members
read the shared team workspace; when the operator creates the task with write
scope, a member can switch to writing in its own worktree (see
[cooperative writers](#godeye-cooperative-writers)). Members can run different
models, bring in consultants and require cross-model review (see
[teams on several models](#godeye-teams-on-several-models)); the operator
decides per model what it may do in a team (see
[model rules](#godeye-model-rules-and-usage-by-model)).
The [terminal guide](15-cooperative-terminal.md) includes a local scripted demo,
controls and an optional native check. Actual outcomes for the maintained source
fingerprint belong to `verification.json`; fixture success does not establish
live-provider quality or native confinement.

Verified with fixtures (`crates/layeragent-storage/src/team.rs` tests,
`crates/layeragent-core/tests/cooperative_runtime.rs`):

- Analyst and reviewer exchange a question and a linked response directly; the
  coordinator does not relay it. One concurrency slot does not deadlock.
- Simultaneous arrivals for a member are incorporated in one activation.
- Sender identity and kind come from the runtime; extra identity arguments are
  rejected by the tool schema, and members cannot send user instructions.
- Idempotency keys are scoped by team, actor and kind; reusing a key with
  different content is a conflict.
- Work has a single owner; dependencies must belong to the team.
- `reviewed` requires another reviewer/verifier, a recorded initial assessment
  and runtime evidence whose expectation was met on the current snapshot.
  Environmental failures cannot reproduce or discard. A newer snapshot marks
  earlier conclusions stale without deleting them.
- Concurrent reservations cannot exceed the token budget; uncertain consumption
  keeps counting. Every fixture provider request was reserved and settled.
- Three equivalent requests without new evidence or finding transitions are
  rejected with a diagnostic. Wait cycles and idle teams receive runtime notices;
  after two notices without progress the team blocks and needs explicit resume.
- Late messages after close are retained as rejected and do not reactivate.
  Close is finalized by the supervisor without a final model call.
- Cancel terminates members and prevents new activations; restart restores the
  team as blocked, fences stale activations and marks open reservations uncertain.

Terminal milestone additions and their verification boundaries:

- Message incorporation commits the sanitized canonical history entry and delivery
  `context_seq` together. Storage tests inject a transaction failure and reopen
  the database to check rollback and deduplication. Legacy incorporated deliveries
  without a context reference require explicit reconciliation.
- A pause lets the current provider call or tool finish and prevents the next
  operation. Snapshot `pause_pending` remains true until safe boundaries are
  acknowledged. Controlled runtime tests exercise inference, tool and approval
  boundaries, queued operator messages, cancellation and resumption. A paused
  team remains paused across restart; live reservations are not rewritten by an
  ordinary resume.
- Member events are attributed through `team_member_event`; member completion
  does not complete the parent task. Pending approvals retain task and activation
  ownership and can be inspected after attaching a new terminal.
- CLI supports explicit execution mode, bounded cooperative options, inspect,
  watch, messages, team controls, priority and JSON/Markdown export. TUI supports
  creation/attachment, a responsive retained panel and controls during execution.
  Attachment does not create or resume work. Cancellation applies to the whole
  team; pause, resume and retirement also apply to one member (see
  [operator controls per member](#godeye-operator-controls-per-member)).
- Snapshots include an event cursor and pending approvals. Exports read one
  consistent transaction and retain delivery receipts, finding revisions and
  attributed tool events, with recursive supported-pattern redaction.
- Writable scope is accepted at creation only when the task policy allows writes;
  writing itself is per member and per activation (see
  [cooperative writers](#godeye-cooperative-writers)). Traditional delegated
  writer worktrees remain a distinct execution path.
- `tests/cooperative_smoke.py` exercises the actual daemon and CLI with a scripted
  loopback provider; `tests/cooperative_tui_smoke.py` exercises the panel and commands
  in a PTY. The default finding is inconclusive because it does not execute a
  native check. The explicit `--native --user-service` variant requires the
  production sandbox and a successful negative/positive control before discard.
  Neither variant measures reasoning quality with a real model.

Reservation bounds are conservative estimates (serialized bytes capped by the
declared window, plus the requested output ceiling); no adapter provides an exact
count. Interrupted in-flight requests retain uncertain reservations until their
consumption is explicitly reconciled; cancellation does not release that capacity.

Pending: web/desktop team interfaces, the
cross-mode evaluator, live-provider evaluation and the complete P0/P1 traceability
table. An initial assessment is recorded but is not an isolation guarantee against
seeing peer conclusions; review checks a different reviewer and linked current
evidence, not independence of every evidence producer. These remain qualification
boundaries rather than claims of a complete independent-audit workflow.

## Prompt caching and cached-token accounting

Agent rounds resend the same prefix, so cached input is now priced and, on
Anthropic, requested; see
[prompt caching and cost](07-multi-provider-resilience-and-model-routing.md#prompt-caching-and-cost).

- Usage carries `cache_read_tokens` and `cache_write_tokens` as parts of
  `input_tokens`, which now always counts every input token (Anthropic's
  fresh, written and read input together). Turn and task usage, task metrics,
  team reservations, team totals and `usage_by_model` record them.
- One cost function prices fresh input, cache reads, cache writes and output
  (cache prices default to the input price), and the provider reserve uses the
  upper bound, every input token at the cache-write price when caching. A
  completion that does not name its model is charged as the selected model.
- `models.NAME.prompt_cache` marks the last tool, the instructions and the
  last conversation block for Anthropic; `setup` enables it for the official
  Anthropic endpoint. Streamed chat requests send
  `stream_options.include_usage`. No migration is needed: stored usage
  without the new fields reads as zero cached tokens.

Verification: a configuration test covers pricing with and without cache
prices, a provider over-reporting cached tokens, the reserve bound and invalid
prices. Provider tests check the three marks and their absence without
`prompt_cache`, the usage request on chat streams, and cached-token parsing for
OpenAI chat, DeepSeek, OpenAI responses, Anthropic streams and documents, and
Gemini, using payloads shaped like each provider's documented usage. A runtime
test charges a two-round turn that writes then reads the cache at the cache
prices. No live provider was called: real cache hit rates depend on each
provider's minimum cacheable size, cache lifetime and prefix stability.

## Godeye team bus, first increment

Godeye is the name for the cooperative team bus: the only channel through which
agents in a team exchange messages. The unused bridge mailbox methods were
removed; the deprecated `bridge` request flag is still refused. This increment
builds on the existing `team_*` storage, identities, idempotency and no-progress
protections.

- **Claims and handoffs.** `team_message` accepts two more purposes. A `claim`
  without an `evidence_id` is accepted but reported as `supported=false` in the
  send result, the `team_message_accepted` event, message listings and threads,
  and the recipient reads it as an unsupported claim. A `handoff` requires a
  recipient and a `work_id` that the sender is working on; the work becomes
  pending for the recipient in the same transaction as the message, and the
  sender is released (`team_work_handed_off`).
- **Readable incorporation.** A delivery enters the recipient's context as a
  short block (authority, sender and role, purpose, message id, references,
  provenance, body) instead of a raw JSON object. Model-written header values are
  kept on one line, and the closing marker names the server-generated message id,
  so a body cannot fake the end of its block.
- **Urgent messages.** With `urgent=true`, a pending message stops the
  recipient's current tool batch before its next call. The skipped calls get
  "not executed" results, so provider tool pairing stays valid, and the next
  round incorporates the message before the model plans again. An activation
  honours at most three urgent interruptions. `layeragent team message --urgent`
  sends one; a TUI correction on a cooperative task is sent to the coordinator as
  urgent. Migration 009 adds the flag.
- **Event-driven wake-up.** The supervisor and paused activations wake on
  committed team changes through a per-team change counter instead of polling
  every 50 ms. A refused write wakes nobody. A 250 ms fallback remains for time
  budgets, leases and pause acknowledgement.
- **Waiting in place for an answer.** `team_wait` with `reply_timeout_secs`
  (1..120) waits inside the current activation for an answer to the member's own
  open question or verification request, so a short exchange does not cost the
  asker a new activation. It returns immediately when no recipient is running and
  no slot is free for one.

Verification: storage and runtime tests cover handoff ownership and transfer,
claim support flags, block rendering against forged fields, urgent interruption
(the test fails with the interruption disabled), change signalling and the
in-place wait with and without a free slot. All of it uses scripted models; it
shows that the mechanisms work, not that a real model chooses well when to
claim, hand off, mark a message urgent or wait. Since the writers increment
below, a handoff from a writing member also carries its unsubmitted changes.

## Godeye cooperative writers

Second godeye increment: team members can change files, one worktree each, and a
change reaches the team workspace only after another member approves it.

- **Read or write, per member.** Every member starts reading the shared team
  workspace. `team_access` with `mode=write`, a `work_id` the member is doing and
  the paths it will change switches it to writing; `mode=read` switches it back.
  Any role can do this, but only in a team created with write scope
  (`--allow-write`, or `scope.allow_write` in the task API), which the gateway
  accepts only when the task policy allows writes. The switch applies from the
  member's next activation, which the supervisor starts for that reason alone;
  an activation keeps the access it started with. Write tools and
  `team_patch_submit` are offered only to writing activations.
- **Own worktree.** The first switch to writing creates a linked Git worktree
  under the runtime state directory whose base commit is the team workspace as it
  is at that moment: HEAD plus uncommitted and untracked, non-ignored changes,
  captured with a private index so the operator's staging area is never touched.
  Secret-looking files are not copied. Writing activations run there; they never
  write the team workspace. A check run with `team_verify` in a worktree does not
  move the team snapshot. The team workspace must be a Git repository with at
  least one commit.
- **Path reservations.** Writing reserves files or directory prefixes for one
  work item. A reservation that overlaps another member's is refused with the
  holder's name and work. Reservations are released when the work is finished,
  cancelled or reopened, when the member switches back to reading (unless a patch
  for that work awaits review), and they move with a handoff.
- **Patches.** `team_patch_submit` has the runtime compute the member's diff
  against its base, refuse paths outside its reservations, secrets, Git metadata,
  symbolic links and leftover conflict markers, and record it with a `patch` evidence receipt
  that a `claim` can cite. Naming a reviewer sends the review request. A newer
  submission supersedes an earlier open one; work whose patch awaits review
  cannot be marked completed around the review. Diffs pass through files, so
  large patches are not truncated (this also fixes the delegated child patch,
  which was cut at 64 KiB). Patches are limited to 2 MiB and 256 files.
- **Review and ordered integration.** `team_patch_show` pages through a redacted
  diff. `team_patch_review` by any other member rejects (the work stays with its
  author, who is told why) or approves. Approving applies the patch to the team
  workspace's working tree, whole or not at all, one integration per team at a
  time and under the same workspace lease as an ordinary task; nothing is
  committed. The integration gets its own receipt, completes the work, releases
  its reservations and marks every worktree as behind. A patch that no longer
  applies is recorded as conflicted and returned to its author. When the policy
  requires approval for `write_file` or `edit_file`, it also does for
  `team_patch_review`.
- **Keeping up.** A worktree that is behind is moved onto the current team
  workspace at the member's next activation, or when it submits; its own changes
  are re-applied with a three-way merge and conflicts are left marked for it,
  with a notice.
- **Handoffs carry work.** A handoff from a writing member carries its
  unsubmitted changes to the work's reserved paths; they leave the sender's
  worktree and are applied to the recipient's worktree when it writes.
- **End of a team.** When a team is terminated, cancelled or fails, each worktree
  is removed and any unsubmitted changes are kept as a `retained` patch. A
  worktree that is behind the team workspace is synced first, so changes that
  were already integrated are not kept again. Paused
  and blocked teams keep their worktrees. `layeragent team patches <task>` and the
  `patches` resource list patches; exports include them. Migration 010 adds
  member access, reservations and patches.

Verification: Git helper tests use real repositories (dirty-workspace bases,
index preservation, secret exclusion, three-way sync after an integration, real
conflicts, all-or-nothing refusal of a stale patch, handoff transfer, refusal to
reset anything but a linked worktree). Storage tests cover access rules,
overlapping reservations, claim coverage, review by another member, superseding,
conflicts, handoff transfer and retained work. Three runtime tests in
`crates/layeragent-core/tests/cooperative_writers.rs` drive a scripted team over a
real repository: a member switches to writing, edits in its worktree and its
approved patch reaches the team workspace next to the operator's uncommitted
work; an operator edit makes the first approval conflict and the author resolves
it; and a handoff carries half-done work into another member's worktree. The
sandbox in those tests is a test double that writes files directly: it does not
certify that the native sandbox confines a worktree writer, and the native
cooperative check was not run for this increment. Scripted models show the
mechanisms work, not that a real model reserves paths, splits work or reviews
well.

## Godeye teams on several models

Third godeye increment: members of one team can run different models, a member
can bring in a consultant on another model for one question, and a team can
require that patches are approved from a different model than the one that
wrote them. The operator-facing description is in
[teams on several models](15-cooperative-terminal.md#teams-on-several-models).

- **Recorded models.** The supervisor fixes each activation's model alias when
  it starts and records it with its model id (lowercase, without a vendor
  prefix). Members without their own model run the team's, recorded as the
  team's `default_model`. Patches record the model of the activation that
  submitted them and of the one that reviewed them. `team_members` shows each
  member's model and the configured aliases with model ids and prices, without
  endpoints, credentials or parameters. Exports list members with their models.
- **Consultants.** `team_consult` adds a `consultant` member with the resolved
  model alias, a token budget and the question as its only delivery. The alias
  must be configured, able to call tools, allowed by the privacy settings, and
  priced when the team has a cost ceiling. Consultants get read tools and the team tools to answer, wait,
  record findings and evidence, read and review patches; the team tools that
  create or take work, switch to writing, add members, consult or close are
  hidden from them and refused by the runtime and by storage. They receive only
  messages addressed to them and do not count against `max_members`. Two can
  run at once and eight over a team's life. When a consultant's activation ends,
  storage retires it if it answered the member that asked, failed, spent its
  budget, or finished without answering; one waiting on its own clarifying
  question, or interrupted by the runtime, stays for at most three activations.
  A retirement resolves the open question and, without an answer, sends the
  asker a runtime notice. A consultant's failure or exhausted budget ends the
  consultation, not the team.
- **Budget.** Before each model call of a consultant, storage compares the
  tokens it has consumed (settled usage, plus reserved estimates for calls whose
  consumption is unknown) with its budget and refuses the call once it is spent.
  A consultation can therefore exceed its budget by at most one call. Team
  limits apply on top.
- **Cross-model review.** With `cross_model_review`, storage refuses an approval
  whose activation model equals the patch author's, both when the review is
  checked and when it is recorded; rejections are not restricted. A submission
  that names a reviewer on the author's model is refused before the request is
  sent. The option needs write scope, and the gateway refuses it unless two
  configured models have different model ids. `team_consult` with a `patch_id`
  makes the consultant's decision the answer to the consultation: the review
  outcome is sent to the member that asked as a response, and to the author.
- **Interfaces.** `--cross-model-review` and `--consultant-model ALIAS` on
  `run` and `/team start`; `team show` lists each member's model and each
  consultation's outcome and token use. Migration 011 adds the recorded models
  and the consultant columns.

Verification: storage tests cover an approval refused on the author's model and
accepted on another, the record repeating that check, recorded author and
reviewer models, a consultant answering and retiring without taking a member
place, broadcasts and work kept away from consultants, a budget refused after
it is spent, a silent consultation reported to the asker, the running and
lifetime consultation limits, and a consultant's patch decision answering its
consultation. A unit test covers the consultant's tool list, including an
operator allow-list that would otherwise become empty. Two runtime tests in
`crates/layeragent-core/tests/cooperative_consult.rs` drive a scripted team on
two configured models over a real repository: the writer's same-model reviewer
is refused before the request, the coordinator consults the other model with
the patch, the consultant reads the real diff and approves it, the patch is
integrated with both models recorded, and the consultant is retired with its
consultation answered; and a model outside the provider allowlist is refused
before it joins, then a consultation that spends its budget on its first call is
refused its second, retired, and the team still completes. Both
scripted models run behind one fixture provider: these tests show the
mechanisms, not that two real providers interoperate in one team, nor that a
different model reviews better. No live run with two providers was made for
this increment. The sandbox is the same test double as for writers and does
not certify confinement.

## Godeye model rules and usage by model

Fourth godeye increment: the operator states per model what it may do in a
team, members see the operator's note on each model, and the team reports what
each model consumed. The operator-facing description is in
[teams on several models](15-cooperative-terminal.md#teams-on-several-models).

- **Team access per model.** `models.NAME.team_access` is `write` (the
  default, the previous behaviour), `read` or `none`. Task creation refuses a
  team model or role model that is `none`, write scope without any `write`
  model, and cross-model review unless two models allowed in teams have
  different model ids. `team_add_member` and `team_consult` refuse a `none`
  model. `team_access mode=write` is refused for a member whose model is
  `read`, and so is a handoff of work with reserved paths to such a member;
  review and approval are allowed on `read` models. The supervisor checks the
  current configuration before every activation and blocks the team with the
  reason when a member's model may no longer run it, or may no longer write
  for a writing member. Checks during an activation use the configuration it
  started with.
- **Notes.** `models.NAME.notes` is one line of at most 200 bytes. With more
  than one model allowed in the team, every member's prompt lists those models
  with their model id, access and note (at most eight; `team_members` lists
  all), and a member on a `read` model is told it reviews and hands over work
  needing changes. `team_members` adds `team_access` and `notes` to each model
  and leaves out `none` models.
- **Usage by model.** The snapshot's `usage_by_model` groups the team's
  activations by recorded model: activations, settled input and output tokens
  and cost, and tokens and estimated cost still reserved or uncertain.
  `team show` and the Markdown export print it. Figures are the runtime's
  accounting with configured prices, not provider invoices.

Verification: a configuration test covers parsing, the default, the three
levels and note validation. A gateway test covers a refused excluded team or
role model, an excluded model not counting toward cross-model review, write
scope without a writer model, and accepted read-only teams. A storage test
covers usage grouped per model, including an interrupted call kept at its
estimate, and the Markdown section; a CLI test covers its rendering. Two
runtime tests in `crates/layeragent-core/tests/cooperative_consult.rs` drive
scripted teams: one checks the prompt's model list and `team_members`, refuses
an excluded model for a new member and a consultation, refuses writing to the
reviewer on a `read` model, which hands the work to the writer, refuses the
writer handing reserved work back, and integrates the writer's patch approved
from the `read` model, with consumption reported per model; the other blocks a
team whose role model became `none` after creation before that model runs.
Scripted models show the rules hold, not that real models choose well between
models, and no live run with two providers was made.

## Godeye operator controls per member

Fifth godeye increment: the operator can pause, resume or retire one member
of a running team without stopping the rest. The operator-facing description
is in [controlling one member](15-cooperative-terminal.md#controlling-one-member).

- **Pause and resume.** `team pause|resume --member` (a `member` on the team
  command API) records an operator hold on that member. Its activation ends at
  its next boundary, so it gives its concurrency slot back; it is no
  candidate, its messages wait and do not count as pending work for the idle
  check, and idle notices name it. A team resume keeps it paused. Resuming
  clears the hold and sends it a runtime notice, which reactivates it even
  when its work is already in progress, and counts as team progress.
- **Retirement.** `team retire` (refused for the coordinator) terminates the
  member in one transaction: its live activation becomes interrupted and its
  reserved budget uncertain, deliveries to it are cancelled, its work returns
  to the coordinator as pending, its path claims are released, its open
  submissions become `withdrawn` and pending handoffs to it `retained`,
  questions it asked are resolved, and questions waiting on it are resolved
  with a runtime notice to their senders. The coordinator is told which work
  and patches were affected. The supervisor then cancels the member's running
  activation, including an in-flight provider call, and once it has stopped
  keeps its unsubmitted worktree changes as a retained patch, under the
  integration lock. An activation stopped by either control ends as
  interrupted and never fails the team. Migration 012 adds the hold columns.

Verification: storage tests cover a pause that leaves the team active, its
replay, the paused activation reaching its boundary and being refused
budget, the member leaving the candidates, the idle notice, a team resume
keeping it paused, the resume notice, a retirement that fences the
activation, keeps its reservation uncertain, returns the work, releases
claims, withdraws the patch, tells the waiting reviewer and the coordinator
and labels the retained patch, the coordinator refused, and a retired
consultant reporting to its asker. CLI tests cover the commands, the name to
id resolution and the terminal syntax. A runtime test in
`crates/layeragent-core/tests/cooperative_controls.rs` drives a scripted team
over a real repository: a writer paused after one edit makes no further model
call while the team goes on, is resumed by notice, edits again and is retired
during a held provider call; the call is cancelled within seconds (the test
fails with the supervisor's cancellation removed, the call then staying open
for its full 20 seconds), the change it made after resuming is kept as a
retained patch, its worktree is removed, the coordinator cancels the returned
work and the team completes. The scripted model plays the operator at fixed
points; this shows the mechanism, not how an operator uses it with real
models.

## Godeye project memory and dead ends

Sixth godeye increment: teams learn what earlier teams on the same workspace
concluded, and an export collects what did not work out. The operator-facing
description is in
[what earlier teams concluded](15-cooperative-terminal.md#what-earlier-teams-concluded).

- **Project memory.** Findings in a concluded state (discarded, inconclusive,
  reviewed, reproduced) of ended teams with the same tenant, principal and
  workspace path are a team's project memory; nothing crosses principals or
  workspaces, and a running team's conclusions are not memory yet. Each entry
  carries its claim, final reason, the snapshot it held for, whether that is
  the asking team's current snapshot (when both are known), the earlier task
  and its latest evidence (output capped), redacted. Every member's first
  prompt lists the eight most recent as earlier peer conclusions, not
  instructions, with claims and reasons reduced to one line of at most 200
  bytes; consultants are not given the list. `team_memory` filters by state or
  text (at most 50 entries). `cooperative.without_project_memory`
  (`--without-project-memory`) starts a team without it, and `team_memory`
  then returns nothing.
- **Dead ends.** The export's `dead_ends` Markdown lists the team's discarded
  and inconclusive findings with their revision history and evidence
  (invocation, exit code, observed output, error), rejected and withdrawn
  patches, and earlier teams' dead ends on the workspace. `team export
  --format dead-ends` prints it, for example into `MUERTOS.md`; `team memory`
  and `GET /v1/tasks/{id}/team/memory` list the project memory.

Verification: a storage test covers memory appearing only once the earlier
team ended, open hypotheses left out, the evidence and snapshot comparison,
state and text filters, a refused state, isolation from another principal on
the same path and from another workspace, and both teams' dead-ends documents.
CLI tests cover the option, the commands and the rendering. A runtime test in
`crates/layeragent-core/tests/cooperative_memory.rs` runs two scripted teams
after an earlier one that discarded a hypothesis with evidence and left
another inconclusive: the next team's first prompt lists both, its coordinator
finds the evidence with `team_memory`, and a team started without project
memory is told nothing. Scripted models show the mechanism, not that a real
model rechecks or uses earlier conclusions well.

## Cooperative bus hardening increment

This increment closes defects found by auditing the existing cooperative
runtime before it becomes the inter-agent message bus. It adds no new
cooperative capability beyond read helpers. Each item has a regression test in
`crates/layeragent-storage/src/team.rs`, `crates/layeragent-core/tests/cooperative_runtime.rs`,
`crates/layeragent-core/src/cooperative.rs` or `crates/layeragent-gateway/src/teams.rs`;
outcomes for the maintained source belong to `verification.json`.

Runtime and lifecycle:

- A runtime stop or graceful shutdown leaves a running team `blocked` for explicit
  resume. Previously the supervisor's own token was cancelled by the stop and the
  team was recorded as `cancelled`, which made it unrecoverable. A paused team
  stays paused.
- Terminal team states (`cancelled`, `terminated`, `failed`) are final: a late
  supervisor write can no longer turn a cancelled team back into a resumable one.
  Runtime state changes now advance the team revision.
- A member is never a candidate while it has a live activation, including after
  `team_wait`. The existing unique index already prevented a second concurrent
  activation; the supervisor no longer retries it every polling interval, and
  capacity is computed from its own live activations.
- An activation admitted by the supervisor is finished as `failed` if its session
  cannot be created or bound, instead of holding a concurrency slot until restart.
- The runtime-derived idempotency key for team appends includes the observed team
  revision, so a deliberate repeat after the team changed is a new operation
  rather than a permanent conflict. Retries of one call remain deduplicated by
  the tool-call journal.
- An added member's model must have configured prices when the team has a cost
  ceiling; unpriced usage would otherwise be charged as zero.

Messages, work and findings:

- Message validation (size, recipient membership, team-scoped references) runs
  before late and rejected dispositions. Self-addressed messages are refused.
- A failed or terminated member cannot receive targeted messages, and its
  accepted deliveries are cancelled when it fails, so the team cannot wait on it
  forever.
- Only a recipient of a question can mark it responded, and open-question checks
  consider only accepted responses from the same team.
- An idle check counts notice rounds, not notice rows: a wait cycle receives a
  second round of notices before the team blocks. An accepted member response is
  progress.
- The coordinator can cancel or mark inconclusive work nobody else can finish
  (unassigned work behind an inconclusive dependency, or work owned by a failed
  member). Work cannot be claimed while the team is not active or reopened while
  it is closing. Clearing a member's current work only affects that work item.
- Findings and their transitions require an active team and team-scoped evidence;
  transition evidence must belong to the finding. A snapshot can only be declared
  current when it is the team's current snapshot. Leaving `reviewed` clears the
  recorded reviewer; the revision history retains it. A message disposition is
  recorded once.

Interfaces:

- Task routes (read, events, stream, metrics, cancel, correct, approve, task
  artifacts, `?task=` artifacts and listing) enforce the identity's workspace
  grant, as team routes already did. A task outside the grant is reported as not
  found on both task and team routes.
- A task-level correction on a cooperative task is refused with a conflict instead
  of being stored and never applied. The TUI delivers text typed during a
  cooperative task to the coordinator as an operator team message.
- `/team attach` discards the previous team's panel before the new snapshot
  arrives, so no control can target the previous team.
- Markdown exports keep model-written text inside its list item, and the CLI
  escapes control and bidirectional-override characters before printing an export.
- Live team views refresh from bounded resource tails
  (`GET /v1/tasks/{id}/team/{resource}?tail=N`, at most 200 rows) instead of
  downloading the complete export on every refresh.

Foundations for the message bus: `GET /v1/tasks/{id}/team/threads/{message}`
returns a reply chain (root and replies, bounded to 500 messages of the team),
and migration 008 adds indexes for reservations by activation and messages by
finding or work item.

Platform and effort fixes found by CI on this increment: Auto effort now
escalates on output consumption as well as rounds (previously a first response
larger than the medium share failed the turn with "turn output token budget
exhausted", which broke the native remote `agent-edit` smoke); the resource-limit
code no longer casts platform-specific `setrlimit` resource types; and the web
surface's UNIX-socket daemon client and its tests are compiled only on UNIX hosts.
Worktree paths are handed to Git without the Windows verbatim prefix, which Git
cannot create directories under.
The daemon refuses a runtime socket path longer than the platform's UNIX socket
limit with an explicit error, and the smokes use the short real `/tmp` on macOS.
These are compile and behavior fixes; macOS and Windows execution stay disabled.

Known open items, not changed by this increment: one member's failed activation
still fails the whole team; activation leases are renewed by heartbeats and are
not reaped on expiry, so a hung activation keeps its slot; the objection debate
limit resets when an objection's claim changes or it becomes inconclusive;
uncertain reservations and deliveries have no reconciliation command; incorporated
peer messages are `user` turns, so under context pressure the activation prompt
can be trimmed before later peer messages; TUI team controls use the revision at
command time rather than the revision displayed in the panel. Native confinement
and live-provider behavior are not qualified by this increment.

## CLI session-control increment

This increment is **partial coverage** of the CLI roadmap. It does not complete
all six milestones or certify new paid controls.

- The terminal command catalog now supplies aliases, help and command-name Tab
  completion. Existing argument handlers remain specialized; subcommand/argument
  completion and a fully typed dispatcher remain pending.
- Enter while an ordinary task is active submits a revision-checked correction in
  the background. The runtime still applies it only at a safe boundary. A conflict,
  finished task or missing task identity restores the draft rather than starting
  another task. Corrections before task admission require an explicit retry.
- `context_status` and `compact_context` are model tools, subject to the existing
  tool allowlist, approval policy, claim/result journal and sanitized event path.
  Neither changes permissions or issues an additional provider request.
- `compact_context` currently creates a **mechanical** checkpoint only, covering
  complete requests before the current request. It preserves canonical messages
  and the current request verbatim. It uses the running turn's existing lease.
- Before inference, estimated compiled input plus the output reserve triggers
  automatic durable mechanical compaction at 80% of the model window when a new
  completed-request boundary exists. This estimate includes instructions, skill
  descriptions, retrieved memory and tools. A long first request or a previously
  checkpointed boundary continues to use the existing bounded in-memory trimming;
  a 60% target and semantic compaction are not implemented.
- The status tool uses the active task's selected model; the human `/context`
  still uses the configured primary model. Both status surfaces retain their
  explicit estimation exclusions below; they are not full-request measurements.

Remaining roadmap: approved durable spending envelopes, semantic compaction,
`set_effort`, `ask_user`, visible task plans, `/init`, agent-initiated teams,
command argument completion, file mentions, multiline/editor input, mode and
permission controls, sessions and panel recovery, operational slash commands,
undo/rewind with attributable file journals, exports, localization and attachments.
`/bridge` and `/ilustration` remain unavailable. No unconfined fallback or new
platform has been enabled.

Verification for the maintained source belongs to [`verification.json`](verification.json).
Fixture tests exercise audited controls, checkpoint persistence, canonical-history
preservation and current-request retention. They do not qualify native confinement
or semantic-summary quality. Native outcomes are recorded separately by the
verifier; blocked and skipped checks are not passes.

## Terminal continuation implementation

The interrupted terminal work has been resumed. This is a partial implementation
of the terminal roadmap, not completion of all release gates. The maintained-source
fingerprint, executed checks, counts and blocked checks are recorded by
[`verification.json`](verification.json); previous reports do not certify this change.

Implemented surfaces:

- `/effort [auto|low|medium|high|xhigh]` reads or stores a session preference.
  The engine applies preferences between operations. A task override takes
  precedence over the session preference, then the configured default. Auto
  increases effort before reaching its initial round ceiling or exhausting its
  current share of the configured output budget, and does not decrease during a
  task, so an Auto turn is never stopped below the operator's output ceiling. Input, output, cost and agent-tree ceilings remain enforced.
- `/context` reports an estimate for compiled history, checkpoints and tool
  definitions, with explicit exclusions for instructions, skills, memory and output
  reserve. It is not a measurement of the complete inference request.
- `/compact` creates an explicitly **mechanical** durable checkpoint while holding
  the workspace lease. Full canonical messages remain stored. Repeating compaction
  at the same boundary reuses the checkpoint. Loading checks the covered history
  hash; restart marks unfinished maintenance failed. Semantic summarization and
  automatic semantic compaction remain pending. Automatic mechanical checkpoints are described above; bounded in-memory trimming remains.
- Opening a TUI session loads stored user and assistant text. Restoring tool panels
  and reconnecting an already running task remain pending. Unknown slash commands
  are rejected locally instead of becoming model prompts. Command-name completion
  and background task corrections are implemented in the increment above.
- Task effort/mode/bridge fields have compatible defaults and participate in the
  idempotency fingerprint; options are inserted atomically with the task. Bridge
  and illustration execution are rejected until their implementations exist.
- Memory recall is bounded by count/bytes and excludes retired facts. Scoped
  storage retirement and listing exist; full terminal memory administration remains
  pending. The unused bridge mailbox methods were removed: agent-to-agent
  messages travel only through the cooperative team bus (godeye). The
  `bridge_messages` table stays in existing databases and is not written.

Provider effort is opt-in through `effort_models`: aliases and concrete supported
levels must be declared by the operator. Request mappings have fixture coverage;
this does not qualify a model. OpenAI Chat uses `reasoning_effort`, while Responses
uses `reasoning.effort` (see the [provider guide](https://developers.openai.com/api/docs/guides/reasoning)).
The Anthropic mapping uses manual thinking and requires an output ceiling larger
than its thinking budget; models requiring adaptive thinking are not qualified by
this mapping (see [extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)).

Still pending: semantic compaction with fully charged usage, complete memory and
context controls, agent mailboxes and `/bridge`, secure remote skill acquisition
and `/ilustration`, general confined MCP transports, argument completion, CLI run
flags and real-provider quality qualification. Existing MCP confinement restrictions
remain in place. No native confinement or live-provider pass is implied by these
terminal changes.

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

An earlier baseline recorded 202 workspace tests and 10 ignored tests. Current
counts and source qualification are recorded in `verification.json`. Ignored
tests require native namespaces, live providers or an independent deployment;
they are not counted as passes without their explicit qualification run.

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
