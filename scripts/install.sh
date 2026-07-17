#!/bin/sh
# Installs (or updates) the gnome-shell-cast daemon for the "Cast to Chromecast"
# GNOME Shell extension. The extension itself is distributed via
# extensions.gnome.org, which cannot ship binaries, so the daemon is installed
# separately by this script.
#
# It downloads a prebuilt, checksum-verified binary from the project's GitHub
# Releases and installs it under your home directory - nothing runs as root.
#
# Usage:
#   curl -fsSL <raw-url>/scripts/install.sh | sh            # latest release
#   curl -fsSL <raw-url>/scripts/install.sh | sh -s -- v3   # a specific version
set -eu

REPO='omid/gnome-shell-cast'
BIN='gnome-shell-cast-daemon'
BIN_DIR="$HOME/.local/bin"
SERVICE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/dbus-1/services"
SERVICE_FILE="$SERVICE_DIR/org.gnome.ShellCast.service"

info() { printf '  %s\n' "$*"; }
step() { printf '\n==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required but not installed."

# 1. Resolve target CPU architecture (must match the release asset names).
arch=$(uname -m)
case "$arch" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported CPU architecture '$arch'. Build from source instead: https://github.com/$REPO#install-from-source" ;;
esac

# 2. Resolve the release tag: explicit "vN"/"N" argument, or the latest release.
version="${1:-}"
if [ -n "$version" ]; then
    tag="v${version#v}"
else
    step "Finding the latest release"
    tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
        | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)
    [ -n "$tag" ] || die "could not determine the latest release (GitHub API unreachable?)."
    info "latest is $tag"
fi

asset="$BIN-$arch-linux"
base="https://github.com/$REPO/releases/download/$tag"

cat <<EOF

This will install the cast daemon to your home directory (no root, no sudo):
  binary:   $BIN_DIR/$BIN   ($asset, $tag)
  service:  $SERVICE_FILE
It is downloaded from GitHub and its SHA-256 checksum is verified before use.
Uninstall later with:  rm -f "$BIN_DIR/$BIN" "$SERVICE_FILE"
EOF

# 3. Download the binary and its checksum, then verify.
step "Downloading $asset ($tag)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fSL --progress-bar -o "$tmp/$asset" "$base/$asset" \
    || die "download failed - does release $tag exist for $arch? See https://github.com/$REPO/releases"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256" \
    || die "checksum file download failed."

step "Verifying checksum"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "$asset.sha256") || die "checksum verification FAILED - not installing."
elif command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "$asset.sha256") || die "checksum verification FAILED - not installing."
else
    warn "no sha256sum/shasum tool found; skipping checksum verification."
fi

# 4. Install the binary and the D-Bus activation service file.
step "Installing"
mkdir -p "$BIN_DIR"
install -m755 "$tmp/$asset" "$BIN_DIR/$BIN"
info "installed $BIN_DIR/$BIN"

mkdir -p "$SERVICE_DIR"
cat >"$SERVICE_FILE" <<EOF
[D-BUS Service]
Name=org.gnome.ShellCast
Exec=$BIN_DIR/$BIN
EOF
info "wrote $SERVICE_FILE"

# 5. Replace any running instance so the next cast activates the new binary,
#    and nudge dbus-broker to pick up the (possibly new) service file.
pkill -x -f "$BIN_DIR/$BIN" 2>/dev/null || true
systemctl --user reload dbus-broker.service 2>/dev/null || true

# 6. Best-effort runtime dependency check (non-fatal).
step "Checking GStreamer/PipeWire runtime plugins"
if command -v gst-inspect-1.0 >/dev/null 2>&1; then
    missing=
    for el in pipewiresrc pulsesrc videoconvert vp8enc opusenc x264enc; do
        gst-inspect-1.0 "$el" >/dev/null 2>&1 || missing="$missing $el"
    done
    # Any one AAC encoder is enough for the HLS fallback.
    if ! gst-inspect-1.0 fdkaacenc >/dev/null 2>&1 \
        && ! gst-inspect-1.0 avenc_aac >/dev/null 2>&1 \
        && ! gst-inspect-1.0 faac >/dev/null 2>&1; then
        missing="$missing <an-AAC-encoder>"
    fi
    if [ -n "$missing" ]; then
        warn "missing GStreamer elements:$missing"
        info "install gst-plugins-good/bad/ugly, gst-libav and pipewire for your distro."
        info "per-distro packages and troubleshooting:"
        info "  https://github.com/$REPO/blob/main/docs/DEPENDENCIES.md"
    else
        info "all required plugins found."
    fi
else
    info "gst-inspect-1.0 not found; skipping (install GStreamer if casting fails)."
fi

cat <<EOF

Done. Open the extension's menu and your Chromecast devices should appear.
If '$BIN_DIR' is not on your PATH, that's fine - D-Bus activates the daemon by
absolute path.
EOF
