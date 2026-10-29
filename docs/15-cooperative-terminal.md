# Cooperative teams in the terminal

LayerAgent can run an individual agent, independent delegated agents, or a
cooperative team whose members exchange durable messages. The terminal exposes
the team's work, questions, decisions and evidence. The current cooperative
milestone is read-only: writer roles and writable scope are refused until their
isolated workspaces and integration flow are implemented.

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

Other commands are `team messages`, `team work`, `team objections`,
`team priority TASK_ID WORK_ID VALUE`, `team close TASK_ID` and
`team cancel TASK_ID`. Query commands support JSON. Member targets accept an
exact ID or an unambiguous exact name. Commands use the observed team revision;
conflicts are displayed instead of silently applying the action to changed state.

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
creates a cooperative task. `/team attach` only observes an existing task;
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
queries include `deliveries` and `finding_revisions`; JSON exports also include
attributed events. Reads and writes retain task, project and principal checks.

Read the [implementation status](10-implementation-status.md) and current
fingerprinted [verification report](verification.json) for executed checks and
blocked qualification. Web/desktop team interfaces, cooperative writers,
investigation memory, cross-mode quality evaluation and live-provider effectiveness
remain separate work.
