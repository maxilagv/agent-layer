# Verification harnesses for the desktop bridge

The desktop shell is a Tauri application. Building it needs the GTK,
WebKitGTK and tray development packages, which are not available on every
host. These harnesses check the same source files without them, so a reviewer
can still verify the bridge.

| Harness | What it checks | Command |
| --- | --- | --- |
| `check-core` | The unit tests of `daemon.rs`, `error.rs` and `state.rs` against the real dependencies | `cd apps/desktop/verification/check-core && cargo test` |
| `check-cmds` | The types of `commands.rs`, `handlers.rs` and `tray.rs` against a stub of the Tauri API | `cd apps/desktop/verification/check-cmds && cargo check` |

Both harnesses include the desktop sources with `#[path]`; they do not copy
them, so they always check the current sources.

Run both with:

```bash
sh apps/desktop/verification/run.sh
```

## What these harnesses do not prove

- The real `#[tauri::command]` expansion, the `generate_handler!` list and
  the window and tray wiring of `src/lib.rs` are not compiled here.
- No window was opened, no tray icon was drawn and no bundle was produced.
- The capability file is not applied by a real Tauri build here.

A qualified desktop release needs a host with the packages listed in
[`../README.md`](../README.md) and a successful `cargo tauri build`.
