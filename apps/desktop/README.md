# LayerAgent desktop app

The desktop app is a Tauri v2 shell around the shared React frontend. It owns
no privilege of its own: the window is a bounded client of the local
LayerAgent daemon, which runs next to it and speaks HTTP over a UNIX socket.

* Window: one main window (`LayerAgent`, 1280x840).
* Tray: shows the state of the daemon and the number of active tasks, with
  the actions *Abrir ventana*, *Detener daemon* and *Salir de la aplicación*.
* Closing the window only hides it. The daemon keeps running and the tray
  keeps reporting its state.
* Plugins: none. There is no shell access, no general filesystem access and no
  process spawning inside the application.

## Layout

```
apps/desktop/
├── README.md                  this document
├── build.sh                   build entry point
├── package.json               optional npm wrapper around the Tauri CLI
├── tools/generate_icons.py    regenerates the PNG icon set
└── src-tauri/
    ├── Cargo.toml             standalone crate, not part of the Rust workspace
    ├── build.rs
    ├── tauri.conf.json
    ├── capabilities/default.json
    ├── icons/                 32x32.png, 128x128.png, 128x128@2x.png, icon.png
    └── src/
        ├── main.rs            entry point
        ├── lib.rs             application wiring
        ├── daemon.rs          bounded client of the daemon
        ├── commands.rs        bounded commands and the event streaming
        ├── handlers.rs        Tauri command wrappers (the public contract)
        ├── error.rs           structured errors returned to the webview
        ├── state.rs           managed state
        └── tray.rs            system tray
```

The crate is a standalone Cargo workspace on purpose: the shared Rust
workspace must keep building while the frontend bundle and the webkit
toolchain are still missing.

## Requirements

Rust (the repository pins the toolchain through `rust-toolchain.toml`) and the
Tauri command line interface:

```bash
cargo install tauri-cli --version "^2"
```

On Debian, Ubuntu and derivatives install the system libraries:

```bash
sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  pkg-config
```

`libgtk-3-dev` is pulled in by `libwebkit2gtk-4.1-dev`; it is required because
the Tauri menu and tray backends are built on GTK.

## Build

The frontend is a separate project and lives in `../../frontend`. A production
bundle embeds the frontend assets, so it has to exist before the bundle is
built.

```bash
# 1. build the shared frontend
cd ../../frontend
npm install
npm run build          # produces ../../frontend/dist

# 2. build the desktop bundle
cd ../apps/desktop
./build.sh bundle      # or: cargo tauri build
```

`./build.sh` runs the same sequence and fails with an explicit message when
`frontend/dist` is missing.

For development, run the frontend dev server and the shell separately:

```bash
cd ../../frontend && npm run dev     # serves http://localhost:5173
cd ../apps/desktop && cargo tauri dev
```

The shell itself also compiles without the frontend bundle, because the assets
are only embedded when the `custom-protocol` feature is enabled (the Tauri CLI
enables it for `tauri build`).

## Run

The daemon has to be running; the shell talks to it over
`<LAYERAGENT_HOME>/runtime/<profile>.sock` and never starts it by itself.

```bash
layeragent serve --ipc-only     # in another terminal
cargo tauri run -- --home "$LAYERAGENT_HOME" --profile default
```

The home directory is `$LAYERAGENT_HOME` when it is set, otherwise the
platform configuration directory (`$XDG_CONFIG_HOME/layeragent` or
`~/.config/layeragent`). The profile defaults to `default`.

## Command contract

The frontend invokes the commands by their snake_case name. Every argument
name is a single lowercase word, so the argument keys are identical in
`snake_case` and in `camelCase`.

| Command | Arguments | Answer |
| --- | --- | --- |
| `bootstrap` | – | installation, daemon health, configuration, models, sessions, tasks |
| `list_projects` | – | `{ "projects": [ { "path", "name" } ] }` |
| `list_sessions` | – | `{ "sessions": [ … ] }` |
| `session_events` | `id`, `after?` | `{ "id", "events": [ … ] }` |
| `list_tasks` | `after?`, `limit?` | `{ "tasks": [ … ], "next", "active_tasks" }` |
| `create_task` | `workspace`, `objective`, `session?`, `model?`, `executor?` | the created task |
| `task` / `get_task` | `id` | the task |
| `task_events` | `id`, `after?` | `{ "id", "events": [ … ] }` |
| `cancel_task` | `id` | the cancellation answer of the daemon |
| `correct_task` | `id`, `instruction`, `revision?` | the correction answer of the daemon |
| `approve` | `id`, `allow`, `revision?`, `hash?` | `{ "recorded": true }` |
| `artifact` | `hash`, `offset?`, `limit?`, `session?`, `task?` | the artifact |
| `list_connections` | – | the list of connections |
| `connection_action` | `id`, `action` | the answer of the daemon (`probe`, `enable`, `disable`, `revoke`) |
| `usage` | – | `{ "totals": { … }, "tasks": [ … ], "task_count", "active_tasks" }` |
| `config` | – | the configuration of the daemon |
| `models` / `list_models` | – | the list of models |
| `mcp_capabilities` | – | `{ "available", "resources", "tools", "prompts", "endpoint", "transport", "response" }` |
| `daemon_status` | – | `{ "reachable", "active_tasks", "daemon": { … } }` |
| `stop_daemon` | `id?` | `{ "stopped", "uncertain_tasks": [ { "id", "objective", "cancellation" } ] }` |
| `subscribe` | `path` | `{ "subscription", "path", "target", "id", "started" }` |
| `unsubscribe` | `id?` | `{ "cancelled", "id" }` |

Example:

```js
const tasks = await invoke('list_tasks', { after: 0, limit: 50 });
const task = await invoke('task', { id: 'tsk_123' });
```

The identifier arguments are accepted under two names, so the caller may use
the short one or the long one:

| Argument | Accepted names |
| --- | --- |
| identifier of a task | `id`, `task_id` |
| identifier of a session | `id`, `session_id` |
| identifier of an approval | `id`, `approval_id` |
| identifier of a connection | `id`, `connection_id` |
| identifier of an artifact | `hash`, `artifact_hash` |
| metadata revision | `revision`, `expected_revision` |
| hash of the reviewed arguments | `hash`, `arguments_hash` |
| session of a new task | `session`, `session_id` |

An approval without an explicit decision is denied: the daemon only runs an
action that was explicitly allowed.

### Failures

A failing command resolves with a structured object, never with a fabricated
answer:

```json
{ "error": "daemon route is not available: /v1/tasks", "unavailable": true }
```

`unavailable` is `true` when the running daemon does not implement the route
yet. `bootstrap` never fails for a missing route: it answers with the parts it
could read and lists the missing ones in `unavailable`.

### Event channels

* `layeragent://task-event` carries the daemon events of a subscription in the
  shape the shared frontend expects (`seq`, `at`, `kind`, `data`).
* `layeragent://event` carries the same events with the subscription metadata
  (`subscription`, `target`, `id`, `seq`, `event`).
* `layeragent://status` carries the result of a stop request, including the
  tasks whose cancellation stayed uncertain.

`subscribe` accepts `/v1/sessions/{id}/events` and `/v1/tasks/{id}/events`.
Every other path is rejected. An empty path cancels every running
subscription; the subscriptions are also cancelled when the window is closed,
so no task outlives the window.

## Authority boundary

* The webview loads only bundled resources. The content security policy of
  `tauri.conf.json` allows `default-src 'self'` plus the IPC origin; no
  remote origin is reachable from the page.
* The capability `capabilities/default.json` grants `core:default` to the
  `main` window and nothing else. The shell installs no plugin, so there is no
  filesystem, shell, process or HTTP permission to grant. The commands of
  this crate are the only privileged operations the page can reach, and each
  of them is bounded: fixed request paths, strict identifier patterns
  (`[A-Za-z0-9_-]{1,64}` for identifiers, `[a-f0-9]{64}` for hashes),
  request timeouts and a hard limit on the size of every answer.
* The grants are enforced by the daemon, not by the user interface. The window
  can only ask the daemon to do something; the daemon decides whether it is
  allowed and whether the effect of a cancellation is known.
* The daemon socket is protected by filesystem permissions. The shell never
  falls back to TCP and never carries a token.

## Icons

The icon set is generated, not downloaded:

```bash
python3 tools/generate_icons.py
```

## Verification status

See [docs/10-implementation-status.md](../../../docs/10-implementation-status.md)
for the maintained status of the repository.

On a host without the WebKit toolchain the crate itself cannot be compiled.
Two harnesses in [`verification/`](verification/) check the same sources
without the system packages:

```bash
sh apps/desktop/verification/run.sh
```

* `check-core` runs the unit tests of the daemon client, the error mapping
  and the state handling against the real dependencies (9 tests).
* `check-cmds` type-checks the commands, the handlers and the tray against a
  small stub of the Tauri API.

`cargo fmt --check` and `cargo metadata` succeed. `cargo check` of the crate
itself is blocked by the missing system packages (`glib-2.0`, `gdk-3.0`,
`webkit2gtk-4.1`, `libayatana-appindicator3`, `librsvg-2.0`, `libxdo`). A
qualified release needs a host with those packages, a successful
`cargo tauri build` and a window that was actually opened.
