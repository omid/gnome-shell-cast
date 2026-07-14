#!/bin/sh
# Sets the single project version N everywhere it must agree:
#   extension/<uuid>/metadata.json   version = N          (EGO integer, tag vN)
#   daemon/Cargo.toml                version = "N.0.0"     (daemon self-report)
#   extension/<uuid>/indicator.js    REQUIRED_DAEMON_VERSION = 'N.0.0'
#   daemon/Cargo.lock                the daemon package entry
# See scripts/release.sh, which bumps N and calls this.
set -eu

UUID='gnome-shell-cast@oxygenws.com'

N="${1:-}"
case "$N" in
    '' | *[!0-9]*)
        echo "usage: $0 <N>   (N a positive integer, e.g. 2)" >&2
        exit 2
        ;;
esac
SEMVER="${N}.0.0"

# Repo root is the parent of this script's directory.
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

meta="extension/$UUID/metadata.json"
cargo_toml="daemon/Cargo.toml"
cargo_lock="daemon/Cargo.lock"
indicator="extension/$UUID/indicator.js"

# metadata.json — integer version.
tmp=$(mktemp)
jq --argjson v "$N" '.version = $v' "$meta" >"$tmp" && mv "$tmp" "$meta"

# Cargo.toml — the package version is the only line starting with `version = `.
sed -i "s/^version = \"[^\"]*\"/version = \"$SEMVER\"/" "$cargo_toml"

# Cargo.lock — the `version` line right after the daemon package's name line.
awk -v ver="$SEMVER" '
    prev == "name = \"gnome-shell-cast-daemon\"" && /^version = / {
        sub(/"[^"]*"/, "\"" ver "\"")
    }
    { print; prev = $0 }
' "$cargo_lock" >"$cargo_lock.tmp" && mv "$cargo_lock.tmp" "$cargo_lock"

# indicator.js — the daemon version the extension requires.
sed -i "s/\(REQUIRED_DAEMON_VERSION = '\)[^']*'/\1$SEMVER'/" "$indicator"

echo "Set version to $N (metadata=$N, daemon=$SEMVER, tag=v$N)."
