#!/bin/sh
# Installs the AppArmor profile that lets bwrap use user namespaces. Requires root.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
here=$(dirname "$0")
for profile in layeragent-bwrap layeragent-chromium; do
  install -m 0644 "$here/$profile" "/etc/apparmor.d/$profile"
  apparmor_parser -r "/etc/apparmor.d/$profile"
done
echo "installed; verify with: layeragent doctor"
