# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the extension and the
daemon ship together under one version.

## [Unreleased]

### Added

- Eleven more interface languages: Arabic, Bengali, Bulgarian, Chinese
  (Simplified), French, Hindi, Indonesian, Portuguese (Brazil), Russian,
  Spanish and Urdu.
- Video encoder and pixel format settings, both automatic by default. Choose
  software encoding when a graphics driver produces a broken or stuttering
  picture, or pin one hardware encoder - VA-API (Intel, AMD), NVENC (NVIDIA) or
  V4L2 (Arm boards) - on a machine that has more than one.
- Cast details now name the encoder and pixel format actually in use, and say
  whether that encoder is hardware or software, so you can see what "automatic"
  chose.
- Preferences now points out when your graphics card could encode but something
  is missing, names the package to install where it knows it, and links to the
  per-card table in the troubleshooting guide. It stays quiet on hardware no
  install would help, such as a virtual GPU.
- NixOS support: a Nix flake builds both halves, with the GStreamer plugins and
  `pactl` bundled into the daemon so no session setup is needed.

### Changed

- The menu entry and the preferences window it opens now both say "Preferences".
- Stream quality and the new encoding settings now live on their own Video page
  in preferences.

### Fixed

- Hardware encoding is now used on Intel graphics for VP9, and on Arm boards
  such as the Raspberry Pi 4 for H.264 - both previously fell back to software
  even though the hardware could encode.
- A cast no longer fails when the system reports an encoder it cannot actually
  start, which happened with a discrete GPU asleep or a stale plugin cache. Each
  hardware encoder is now opened before it is used, and skipped if it will not.
- Casting failed to start at all on machines with VA-API installed
  (`gst-plugin-va`): the hardware H.264 encoder was offered to the device and
  then could not be used.
- Without a hardware encoder, the HLS fallback produced a picture that
  Chromecast receivers cannot decode.
- German and Persian are now used in every region, not only Germany and Iran.

## [3] - 2026-08-14

### Added

- Choose what to cast: a second action on each device that opens GNOME's
  Display/Window picker, alongside the one-click screen cast.
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md), linked from the preferences About
  page.

### Changed

- Each device is now a single row with its cast actions as buttons, instead of a
  submenu.

### Fixed

- Mirroring no longer shows a black screen or drops after a few seconds.
- Casts no longer fail with "Network is unreachable" or fall back to HLS when a
  device announces only its IPv6 address.
- Stopping the screen share from GNOME's orange indicator now ends the cast.
- The panel icon and quick-settings toggle light up as soon as a cast starts.
- Starting a new cast no longer races the previous one's teardown.
- The extension no longer fails to load at login on GNOME Shell 50.
- Error messages no longer leak documentation URLs or `os error` numbers.

## [2] - 2026-08-05

Initial release.

## [1] - 2026-07-15

Initial release.

[unreleased]: https://github.com/omid/gnome-shell-cast/compare/v3...HEAD
[3]: https://github.com/omid/gnome-shell-cast/compare/v2...v3
[2]: https://github.com/omid/gnome-shell-cast/compare/v1...v2
[1]: https://github.com/omid/gnome-shell-cast/releases/tag/v1
