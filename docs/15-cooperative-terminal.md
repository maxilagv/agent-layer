# Cooperative teams in the terminal

LayerAgent can run an individual agent, independent delegated agents, or a
cooperative team whose members exchange durable messages. The terminal exposes
the team's work, questions, decisions and evidence. Members read the shared
workspace; with `--allow-write`, any member can switch to writing in its own
worktree, and its changes reach the workspace only when another member approves
them (see [Writing members](#writing-members)).

## First reproducible run

Build from the repository root using the pinned Rust toolchain:

```bash
cargo build -p layeragent-cli --locked
python3 tests/cooperative_smoke.py
python3 tests/cooperative_tui_smoke.py
```

These commands start a disposable local daemon and a scripted HTTP provider on
loopback. They use no API credentials and make no paid requests. The CLI scenario
creates a team, pauses it during a provider call, sends an operator instruction,
resumes it, observes an objection and changed conclusion, exports the result and
checks restart behavior. The PTY scenario exercises the actual terminal interface.

The scripted investigation concerns the [toy gateway](../examples/cooperative-demo/check.py).
An analyst suspects unauthenticated access because storage contains no local
authorization check. A reviewer identifies the missing premise: the gateway
might reject the request before storage. A verifier receives a direct request.
In the default fixture-only run, no gateway command is executed and the finding
becomes **inconclusive**. A script deciding which tool to call does not establish
real-model reasoning quality.

On a Linux host with the documented confinement prerequisites and an already
delegated systemd user manager, run the stronger scenario:

```bash
python3 scripts/run_native_tests.py --user-service
python3 tests/cooperative_smoke.py --native --user-service
```

This variant runs `python3 -B check.py` through the production sandbox. Both the
anonymous rejection and authenticated positive control must pass. The runtime
records the command, snapshot and observed output; the finding changes to
**discarded** using that receipt. A failed confinement check fails the test; there
is no unconfined fallback. This remains a scripted-model test of a toy case.

To retain the sanitized JSON export, supply `--output demo-result.json` to either
CLI scenario. The path must not already exist. Exports can contain project
content; redaction covers registered credentials and supported patterns, not
every possible confidential value. Review an export before sharing it.

## Use a configured provider

Follow the [quick start](../README.md#quick-start) to configure your model and
qualify the host. Unlike the fixture commands, these commands use the configured
provider and can incur usage:

```bash
layeragent run "Investigate the authorization path" --workspace . \
  --execution cooperative --template audit --max-tokens 200000 --max-seconds 600
layeragent team show TASK_ID
layeragent team watch TASK_ID
layeragent team message TASK_ID --to reviewer "Check the gateway premise first."
layeragent team pause TASK_ID --reason "Inspect pending evidence"
layeragent team resume TASK_ID
layeragent team findings TASK_ID --json
layeragent team evidence TASK_ID --json
layeragent team export TASK_ID --format markdown > investigation.md
```

`--execution` accepts `individual`, `delegated` and `cooperative`; the compatible
default remains `delegated`. Cooperative templates are `audit`, `debug` and
`review`. Token/time limits bound the team; `--max-cost-usd` additionally requires
usable configured model prices. Unspecified limits retain the existing bounded
defaults. Reservation estimates are conservative, not exact provider token counts.

The initial team contains coordinator, analyst, reviewer and verifier roles.
Members have separate contexts. Messages preserve sender identity and provenance;
peer messages do not grant permissions or approve actions. Terminal messages are
operator instructions and still cannot expand the task's permissions.

`team message --urgent` stops the member's current tool batch so it reads the
instruction before its next tool call; without it, the instruction is read at the
start of the member's next round.

## Writing members

```bash
layeragent run "Rename the session helper and update its callers" --workspace . \
  --execution cooperative --template debug --allow-write
layeragent team show TASK_ID      # who writes and which paths each reserves
layeragent team patches TASK_ID   # submitted, handed-over and retained patches
```

`--allow-write` needs a task policy that allows writes and a Git repository with
at least one commit. Without it the team only reads. With it:

1. A member claims a work item and calls `team_access` with `mode=write`, the
   work id and the files or directories it will change. Overlapping reservations
   of another member are refused, naming who holds them.
2. Its next activation runs in its own worktree, which starts from the workspace
   as it is (including uncommitted and untracked files, except secret-looking
   ones). Only writing activations get write tools.
3. `team_patch_submit` records its changes as a patch with a runtime receipt and
   can send the review request. Changes outside its reservations are refused.
4. Another member reads it with `team_patch_show` and decides with
   `team_patch_review`. Approving applies the patch to the workspace's working
   tree whole or not at all, one approval at a time; nothing is committed.
   A rejected or no-longer-applicable patch goes back to its author.
5. When the workspace changes, worktrees are moved onto it and their own changes
   are re-applied; conflicts are left marked for the member to resolve.

`team_access` with `mode=read` switches a member back. A handoff from a writing
member carries its unsubmitted changes to the recipient's worktree. When the team
ends, worktrees are removed and unsubmitted changes are kept as retained patches.
Your own uncommitted work in the workspace is preserved: integration only applies
the approved patch.

Other commands are `team messages`, `team work`, `team objections`,
`team priority TASK_ID WORK_ID VALUE`, `team close TASK_ID` and
`team cancel TASK_ID`. Query commands support JSON. Member targets accept an
exact ID or an unambiguous exact name. Commands use the observed team revision;
conflicts are displayed instead of silently applying the action to changed state.

## Teams on several models

A team can mix models. Each configured model alias (see the annotated
[example configuration](../examples/config.toml)) can run members: the team's
own model runs members without one, `team_add_member` accepts `model`, and
`cooperative.role_models` sets a model per role. `team_members` shows every
member's model and the configured aliases, with their model ids and prices,
never endpoints or credentials. `team show` and exports list each member's model.

```bash
layeragent run "Review and fix the parser" --workspace . \
  --execution cooperative --template review --allow-write \
  --cross-model-review --consultant-model second
```

- **Consultants.** Any member can call `team_consult` with a question and,
  optionally, a model alias, a token budget (`max_tokens`, 5 000 to 600 000;
  150 000 by default), a work item or a patch. The runtime brings in a
  consultant: a read-only member on that model (by default the team's
  `consultant` role model, then the team's model) that receives the question as
  its first message. It answers the member that asked, and is then retired by
  the runtime. It can ask that member one clarifying question and wait for the
  reply. It never takes work, switches to writing, adds members, consults
  others or closes the team, never receives broadcasts, and does not count
  against `max_members`. Up to two consultations run at once and eight over the
  life of a team. The model must be configured, able to call tools, allowed by
  the privacy settings (`allowed_providers`, `local_models_only`) and priced when
  the team has a cost ceiling. A consultant that ends without answering, fails or spends its
  budget is retired too, its question is resolved, and the member that asked
  gets a runtime notice; the team carries on.
- **Cross-model review.** `--cross-model-review` (`cooperative.cross_model_review`
  in the API) needs `--allow-write` and at least two configured models with
  different model ids. A patch is then approved only by a member or consultant
  whose activation runs a different model than the one whose activation
  submitted it; anyone may still reject it. The model of every activation is
  recorded when it starts, and each patch records its author's and reviewer's
  model. Models are compared by id, lowercase and without a vendor prefix, so
  two aliases or providers serving the same model id count as the same model.
  `team_consult` with a `patch_id` brings in a reviewer on another model whose
  decision answers the consultation.

A consultation's budget is checked against actual consumption before each of
its model calls, so the call that crosses the budget completes: a consultation
can exceed its budget by at most one call. Calls whose consumption is unknown
count at their reserved estimate. The team's token, cost and time limits still
apply to every consultation.

## Interactive terminal

```text
/execution cooperative
/team start audit --max-tokens 200000 --max-seconds 600 -- Investigate authorization
/team
/team message reviewer Check the gateway premise first.
/team pause
/team resume
/team findings
/team evidence
/team attach TASK_ID
```

`/execution` selects the mode for subsequent prompts. `/team start` explicitly
creates a cooperative task; add `--allow-write` before `--` to let members write. `/team attach` only observes an existing task;
resuming blocked execution requires `/team resume`. The panel opens automatically,
uses a side view in wide terminals and a switchable view in compact terminals,
and remains available after completion. Approvals take visual priority and name
the member requesting the action.

## State and recovery

- **Pausing:** the current provider call or tool is allowed to finish. No next
  operation starts; the panel shows the pending pause until members reach safe
  boundaries. Team controls remain available. Pauses apply to the entire team.
- **Paused:** messages can remain queued. Resuming preserves live reservations;
  an ordinary pause does not turn known consumption into uncertain consumption.
- **Blocked:** the reason explains restart reconciliation, lack of progress or
  another limit. Resumption is explicit and does not erase spent budget.
  The active-time ceiling interrupts an in-flight operation when exhausted;
  draining operations count, fully paused time does not, and uncertain outcomes
  remain charged.
- **Closed:** the supervisor finalizes the team without a final model call. Open
  work must be disposed of first. Late messages have an explicit disposition.
- **Cancelled:** pending deliveries cannot reactivate members. Interrupted effects
  and uncertain usage remain visible.

`team watch` and TUI attachment never create another task. Ctrl+C in `team watch`
detaches the observer; cancellation is an explicit team command. Exiting the TUI
leaves the daemon's task intact. Reopening the terminal loads a consistent snapshot
and follows events from its cursor. A daemon restart requires explicit recovery;
an already paused team stays paused.

Members exchange questions, observations, objections, verification requests,
responses, claims and handoffs. A claim without an evidence receipt reaches its
recipient marked as unsupported. A handoff moves the sender's in-progress work
item, its reserved paths and its unsubmitted changes to the recipient. A member
can wait in place for an answer to its own open question instead of ending its
activation, when a slot is free for the answer.

Message acceptance, incorporation into context, response and resolution are
different observations. Incorporation and the canonical history entry commit
together. Legacy incorporated deliveries without a context reference are marked
for reconciliation, not replayed blindly. Finding history and uncertainty survive
export; a successful member turn is not completion of the whole team.

## API and qualification boundary

The existing task creation endpoint accepts `execution` and `cooperative` options.
`GET /v1/tasks/{id}/team` returns the snapshot with `last_event_seq`,
`pause_pending` and pending approvals. The existing task event stream includes
`team_member_event` envelopes identifying the member and activation. Team resource
queries include `deliveries`, `finding_revisions`, `patches` and `claims`; JSON
exports also include attributed events and patches. Creating a task with
`cooperative.scope.allow_write` requires a task policy that allows writes, and
`cooperative.cross_model_review` requires write scope and two configured models
with different model ids. Member rows carry `last_model` and, for consultants,
`consult` (who asked, the question, budget, tokens used and outcome); the
snapshot carries `default_model`. `GET /v1/tasks/{id}/team/{resource}?tail=N` returns the most
recent rows of a resource (at most 200), and
`GET /v1/tasks/{id}/team/threads/{message}` returns the reply chain containing a
message. A task-level correction on a cooperative task is refused; send an
operator instruction as a team message instead. Reads and writes retain task, project and principal checks.

Read the [implementation status](10-implementation-status.md) and current
fingerprinted [verification report](verification.json) for executed checks and
blocked qualification. Web/desktop team interfaces, native confinement checks of
worktree writers, investigation memory, cross-mode quality evaluation, live
teams that mix providers and live-provider effectiveness remain separate work.
