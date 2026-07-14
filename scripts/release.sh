#!/bin/sh
# Cuts a new release. Bumps the single project version (see set-version.sh),
# runs the checks, builds the extension zip, commits, tags vN and pushes.
# Pushing the tag triggers .github/workflows/release.yml, which builds the
# per-arch daemon binaries and publishes the GitHub Release. Every step asks
# for confirmation; override the computed version with `V=<n>`.
set -eu

UUID='gnome-shell-cast@oxygenws.com'

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

confirm() {
    printf '%s [y/N] ' "$1"
    read -r ans || ans=
    case "$ans" in
        y | Y | yes | YES) return 0 ;;
        *) echo 'Aborted.' >&2; exit 1 ;;
    esac
}

meta="extension/$UUID/metadata.json"

# Preflight: a clean tree keeps the release commit to just the version bump.
if [ -n "$(git status --porcelain)" ]; then
    echo 'error: working tree is not clean. Commit or stash first.' >&2
    exit 1
fi

cur=$(jq -r '.version' "$meta")
new="${V:-$((cur + 1))}"
echo "Current version: v$cur"
echo "New version:     v$new"
confirm "Release v$new?"

echo
echo '==> Setting version'
sh scripts/set-version.sh "$new"

echo
echo '==> Running checks (make ci + eslint)'
if ! make ci eslint; then
    echo 'error: checks failed. Revert the bump with: git checkout -- .' >&2
    exit 1
fi

echo
echo '==> Building the extension zip'
make ego-zip

echo
echo '==> Changes to commit:'
git add "$meta" daemon/Cargo.toml daemon/Cargo.lock "extension/$UUID/indicator.js"
git --no-pager diff --cached --stat
confirm "Commit these as \"Release v$new\"?"
git commit -m "Release v$new"

echo
echo "==> Tag v$new and push (this triggers the release workflow — point of no return)"
confirm "Tag v$new and push to origin/main now?"
git tag "v$new"
git push origin main "v$new"

cat <<EOF

Released v$new.
- CI is now building the daemon binaries and publishing the GitHub Release.
- Upload the extension to extensions.gnome.org once CI finishes:
    $UUID.v$new.zip
  at https://extensions.gnome.org/upload/   (or run: make shexli)
EOF
