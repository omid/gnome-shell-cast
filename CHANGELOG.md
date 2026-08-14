# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the extension and the
daemon ship together under one version.

## [Unreleased]

## [3] - 2026-08-14

### Fixed

- **Mirroring showed a black screen, then dropped after a few seconds.** Three
  faults compounded: RTP packets were discarded locally whenever a frame's burst
  filled the socket buffer (thousands per minute, shredding the opening key
  frame); the retransmit history was wiped by a frame-id sign error, so the
  receiver's requests for the missing packets could never be answered; and VP8,
  VP9 and AV1 only emitted a key frame every 3000 frames — 75 seconds at 40fps —
  so a decoder that lost sync stayed black until then.
- **Casts failed with "Network is unreachable", or silently fell back to HLS
  even though the device had accepted VP9.** A device announcing only its IPv6
  address — common for a few minutes after a suspend — stranded the cast on an
  address the machine had no route to. Discovery now remembers every address a
  device announces, prefers one that is actually reachable, and never downgrades
  a working address; the media socket matches the device's address family.
- **Stopping the screen share from GNOME's orange indicator left the cast
  running.** The picture froze on the last frame while the extension still said
  "casting". The cast now ends when the compositor ends the share.
- **The panel icon and quick-settings toggle did not light up while casting**
  until the menu was opened, and the cast volume slider could move on its own.
- **Starting a new cast could race the previous one's teardown**, leaving the
  receiver with a dead or duplicated capture.
- **The extension could fail to load at login on GNOME Shell 50**, which builds
  its quick-settings items asynchronously.
- **Error messages no longer leak internal detail** — no more library
  documentation URLs or `os error` numbers in notifications.

### Added

- **Choose what to cast**: a second action on each device that opens GNOME's own
  Display/Window picker, alongside the existing one-click screen cast.
- **[TROUBLESHOOTING.md](TROUBLESHOOTING.md)**, linked from the preferences
  About page — including the most common surprise, a picture cropped on all four
  sides, which is the TV's overscan rather than the cast.

### Changed

- Each device is now a single row with its cast actions as buttons, instead of a
  submenu.
- Devices are discovered with mdns-sd 0.21; dependencies updated.

## [2] - 2026-08-05

Initial release.

## [1] - 2026-07-15

Initial release.

[unreleased]: https://github.com/omid/gnome-shell-cast/compare/v3...HEAD
[3]: https://github.com/omid/gnome-shell-cast/compare/v2...v3
[2]: https://github.com/omid/gnome-shell-cast/compare/v1...v2
[1]: https://github.com/omid/gnome-shell-cast/releases/tag/v1
