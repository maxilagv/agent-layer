#!/bin/sh
# Type-check and unit-test the desktop bridge without the Tauri system packages.
#
# The desktop shell needs GTK, WebKitGTK and the tray library, which are not
# installed on every build host. These two harnesses check the same source
# files with the same dependencies:
#
#   check-core  runs the unit tests of the daemon client, the error mapping
#               and the state handling against the real dependencies;
#   check-cmds  type-checks the commands, the handlers and the tray against a
#               small stub of the Tauri API, so the bridge compiles even where
#               the desktop runtime cannot be built.
#
# Usage: sh apps/desktop/verification/run.sh
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
verification="$root/apps/desktop/verification"

cd "$verification/check-core"
cargo test

cd "$verification/check-cmds"
cargo check
