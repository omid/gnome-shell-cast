# GNOME Shell Cast

Two halves that talk over the D-Bus session bus (`org.gnome.ShellCast1`):

- `daemon/` — Rust. Discovery (mDNS), Cast control (CASTv2), portal capture,
  GStreamer encoding, Cast Streaming (mirroring) with an HLS fallback.
- `extension/` — GJS. Panel/quick-settings indicator, menu, preferences.

The daemon is D-Bus activated and exits after 10 minutes idle.

## Before handing work over

`make check-all` is what CI runs. **Formatting is checked separately from
linting** — `cargo clippy` and `make eslint` both pass happily on unformatted
code, and `check-all` will still fail. Run `make fix-all`, or at least
`make fmt` and `make fmt-js`.

`make shexli` validates the extension against the extensions.gnome.org review
rules. It should stay at 0 errors / 0 warnings.

## Making a change take effect

See the `install-verify` skill. The short version: the daemon needs a rebuild,
install and `pkill`; extension **JS needs a logout** (Wayland, and the shell
caches ES modules per process), while `stylesheet.css` reloads on
disable/enable. Prefer CSS while iterating on appearance.

## Extension code is held to upstream rules

Before writing or reviewing anything under `extension/`, read the pages listed
in the `extension-guidelines` skill: GJS best practices, the extensions.gnome.org
review guidelines, and the EGO AI reference. Answer "does this follow the
guidelines?" from those, not from memory.

## Conventions

- **UI text is sentence case**, not Title Case — menus, prefs, notifications.
- **Comments are minimal**: one line, only where the code cannot say it. No
  block comments restating what the next line does.
- **New user-visible strings go through `_()`**, then `make translations`, then
  fill in every `po/*.po` — do not leave them empty. The `translations` skill
  covers picking wording that matches the rest of the GNOME desktop.
- **Public GNOME Shell API only.** No `_private` members of shell objects; when
  the only route is a private one, guard it so a failure cannot abort
  `enable()` and cost the user the whole extension.
- **Error messages reaching the user must be readable.** The daemon's
  `user_message()` (session.rs) strips crate detail — rustls doc URLs, `os
  error` numbers — into a plain sentence. Keep the detail in the journal.

## Things that have bitten us

Written down because each one cost hours and none of them looked like what it
was. `TROUBLESHOOTING.md` has the user-facing versions.

- **Sockets must match the peer's address family.** An IPv4-bound `UdpSocket`
  connecting to an IPv6 device fails with `EAFNOSUPPORT` and silently drops
  mirroring to HLS. Use `net::connected_udp()`, which also doubles as a
  reachability probe.
- **One mDNS resolve can carry a partial record.** After a resume the AAAA
  often arrives minutes before the A. Discovery accumulates every announced
  address and prefers a *reachable* one.
- **A non-blocking UDP socket returns `WouldBlock` on send.** Dropping those
  packets loses whole frames; a lost key frame is a black screen until the next
  one. `send_packet()` waits instead.
- **Cast receivers ack "nothing yet" as frame -1** (255 truncated). Expanding
  that forward instead of backward retires the retransmit history and disables
  recovery entirely.
- **GNOME Shell 50 builds quick-settings indicators asynchronously**, so
  anything that walks the grid at `enable()` time can find it empty.
- **`g-signal` hands the handler `(proxy, sender, signalName, parameters)`** —
  the payload is the *fourth* argument and a GVariant. Destructuring the third
  silently yields the characters of the signal name.
- **GNOME Shell has no tooltip API.** The dash and the screenshot UI each
  hand-roll a label in the uiGroup. Anything built that way positions itself by
  hand and hardcodes its colours, so it will not follow the user's theme.
