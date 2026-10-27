#!/bin/sh
# Install the Linux build dependencies of the Tauri desktop application.
#
# The desktop shell needs the WebKitGTK stack, a system tray library and the
# SVG loader. These packages are only required to build the desktop
# application; the runtime itself does not need them.
#
# Usage: sudo sh scripts/install_tauri_deps.sh
set -eu

PACKAGES="libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev pkg-config"

if ! command -v apt-get >/dev/null 2>&1; then
    echo "this script installs Debian/Ubuntu packages; install the equivalents by hand on other distributions" >&2
    exit 2
fi

if [ "$(id -u)" -ne 0 ]; then
    echo "administrator rights are required" >&2
    exit 2
fi

apt-get update
apt-get install -y $PACKAGES

echo "installed: $PACKAGES"
echo "verify with: pkg-config --modversion webkit2gtk-4.1"
