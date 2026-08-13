---
name: install-verify
description: Build, install and verify a change to the gnome-shell-cast daemon or extension on this machine. Use after editing daemon/ or extension/ when the change needs to actually run - covers the dbus-broker reload, the daemon restart, and the fact that extension JS needs a logout while CSS does not.
---

# Installing and verifying a change

## Daemon (Rust)

```sh
make install-daemon                          # builds release + installs to ~/.local/bin
systemctl --user reload dbus-broker.service  # only needed if the .service file changed
pkill -f '^/home/omid/.local/bin/gnome-shell-cast-daemon'
```

**Anchor the `pkill` pattern with `^`.** An unanchored `-f gnome-shell-cast-daemon` also
matches the shell running the command, killing it mid-script (exit 144).

The daemon is D-Bus activated, so it restarts on the next call. Verify:

```sh
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 GetVersion
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 ListDevices
md5sum daemon/target/release/gnome-shell-cast-daemon ~/.local/bin/gnome-shell-cast-daemon
```

Discovery takes a few seconds; devices appearing in `ListDevices` means mDNS works.

## Extension (GJS)

```sh
make install-extension
```

Then, depending on what changed:

| Changed | To take effect |
|---|---|
| `stylesheet.css` | `gnome-extensions disable … && gnome-extensions enable …` |
| any `.js`, `metadata.json` | **log out and back in** |

GNOME Shell caches an extension's ES modules for the life of the process, so
disable/enable does **not** reload changed JS, and Wayland has no `Alt+F2 r`.
It does load and unload the stylesheet, so keep visual iteration in CSS.

Confirm the shell is really running new code by comparing timestamps:

```sh
ps -o lstart= -p $(pgrep -x gnome-shell | head -1)
stat -c '%y' ~/.local/share/gnome-shell/extensions/gnome-shell-cast@oxygenws.com/lib/castMenu.js
```

If the install is newer than the shell start, the change is **not** loaded yet.

## Checking the extension loaded

```sh
gnome-extensions info gnome-shell-cast@oxygenws.com   # want State: ACTIVE
journalctl --user -b --since "-2 min" -t gnome-shell | tail
```

`Enabled: Yes` with `State: INACTIVE` is normal **while the screen is locked** —
the extension does not declare the `unlock-dialog` session mode. Check with
`loginctl show-session <id> -p LockedHint` before hunting for a bug.

## Before handing anything over

`make check-all` is what CI runs. Note that formatting is checked separately
from linting: `make eslint` and `cargo clippy` both pass on unformatted code.
Run `make fmt` and `make fmt-js` (or just `make fix-all`).
