# 15 · Web surface, shared frontend and desktop application

> LayerAgent 0.1.0 · Linux development release. Local operator surfaces only.

## What exists

| Surface | Entry point | Transport | Authority |
| --- | --- | --- | --- |
| Web | `layeragent web` | HTTP on a loopback address | The daemon over its UNIX socket; the browser never holds a bearer token |
| Desktop | `apps/desktop` | Tauri commands over the daemon UNIX socket | The daemon; the window may only ask the bounded commands |
| Shared frontend | `frontend/` | Served by the web surface or bundled by the desktop shell | None of its own; every action is a daemon call |

All three read the same persisted state. A task created in one surface is
visible in the others, and closing a surface never stops the daemon.

## The web surface

```bash
layeragent web --bind 127.0.0.1:9474
```

The command starts the daemon if it is not running and then prints a
single-use access code:

```
LayerAgent web interface: http://127.0.0.1:9474
Access code (single use): 59513AAB
```

Open the address, enter the code, and the browser receives an `HttpOnly`
session cookie. A new code is printed after every successful sign-in, so a
second tab can be opened without restarting the server.

Rules the surface enforces:

- the address must be a loopback address; anything else is refused;
- the `Host` header must name this server and an `Origin` header, when
  present, must match it;
- the session lives in an `HttpOnly; SameSite=Strict` cookie and never in
  browser storage;
- every mutation carries an `X-LayerAgent-CSRF` header with the token that
  the bootstrap response handed to the page;
- a content security policy restricts the page to its own resources;
- the sign-in backdrop is rendered locally with WebGL2 and falls back to the
  static CSS gradient when WebGL2 is unavailable; it loads no remote assets and
  honors `prefers-reduced-motion`;
- requests are authorized again on the server before a task or an artifact is
  reached; a stale correction or approval keeps its conflict status;
- signing out invalidates the interface session only. Running work continues.

The frontend is served from `frontend/dist`. When it is missing, the surface
answers with an explicit message instead of an empty page.

## The shared frontend

```bash
export PATH="$HOME/.local/node22/bin:$PATH"   # Node 22 is required
cd frontend
npm install
npm run build
npm test
```

Node 18 is not sufficient for the current Vite release; the package manifest
declares `engines.node` and `.npmrc` turns that into a hard failure instead of
a confusing build error.

The frontend contains the views for projects, conversations, tasks and the
executor, approvals with the full decision context, diffs, artifacts,
connections, usage and settings. It provides:

- keyboard navigation with a visible focus ring, a skip link and an
  `aria-live` region for connection state;
- an explicit empty state, an error state with a retry action and a
  reconnecting banner;
- the snapshot-then-events pattern: a view first loads a snapshot and then
  applies the events that follow it, ordered by sequence number;
- a project change that disposes the previous subscription and clears the
  previous project data before the new snapshot is requested.

## The desktop application

The desktop shell is in `apps/desktop`. It embeds the same frontend and talks
to the daemon over the profile UNIX socket.

Authority is bounded per window and per command:

- the window loads only resources that are part of the application;
- the Tauri capability file grants the application's own commands and nothing
  else; there is no shell plugin and no general file system plugin;
- every command validates its arguments before they reach the daemon;
- the grants, approvals and workspace boundaries stay enforced by the daemon,
  never by the user interface.

Lifecycle:

- closing the window leaves the daemon working;
- the tray shows the daemon state and the number of active tasks;
- *Quit the application* closes the window and the tray;
- *Stop the daemon* requests cancellation and shows the possible uncertain
  effects instead of killing the process;
- automatic start at login is optional and disabled by default.

### Building the desktop application

The Tauri system packages are required on the build host:

```bash
sudo sh scripts/install_tauri_deps.sh
```

Then build the frontend and the shell:

```bash
export PATH="$HOME/.local/node22/bin:$PATH"
cd frontend && npm install && npm run build && cd ..
cargo tauri build --config apps/desktop/src-tauri/tauri.conf.json
```

The first qualified platform publishes a `.deb` package. Other distributions
can build from the source with their own prerequisites.

### Verifying the bridge without the desktop toolchain

On a host without the WebKit packages the desktop crate cannot be compiled.
Two harnesses in `apps/desktop/verification/` check the same sources:

```bash
sh apps/desktop/verification/run.sh
```

`check-core` runs the unit tests of the daemon client, the error mapping and
the state handling; `check-cmds` type-checks the commands, the handlers and
the tray against a stub of the Tauri API. They do not prove that a window
opens or that a bundle is produced.

## What the frontends never receive

- the daemon bearer token of the local runtime;
- remote pairing keys;
- a direct connection to the remote coordinator.

The daemon presents remote job metadata and the runner that owns them. The
browser and the WebView only see the metadata the daemon chose to publish.
