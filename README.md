# GNOME Shell Cast

Cast your **whole screen or a single window** to a **Chromecast** device, right from the GNOME Shell top panel.

The project has two parts, shipped together in this repository:

| Component | Language | Role |
|---|---|---|
| `extension/` | GJS | Panel indicator: device list, *Cast Screen* / *Cast Window*, *Stop Casting*, preferences |
| `daemon/` | Rust | Does the heavy lifting: Chromecast discovery (mDNS) and control (CASTv2), screen/window capture (XDG ScreenCast portal → PipeWire), encoding (GStreamer, H.264 + AAC), and serving the live HLS stream over HTTP |

The two talk over the D-Bus session bus (`org.gnome.ShellCast1`). The daemon is D-Bus activatable, so it starts on demand and exits when idle.

## How it works

Chromecast's Default Media Receiver plays media it pulls over HTTP. When you start a cast:

1. The daemon opens an XDG ScreenCast portal session — GNOME shows its native picker for a monitor or a window.
2. A GStreamer pipeline captures the PipeWire stream, encodes H.264 video and AAC system audio, and writes a live HLS stream into `$XDG_RUNTIME_DIR/gnome-shell-cast/`.
3. A tiny built-in HTTP server serves that stream on your LAN.
4. The daemon tells the Chromecast (CASTv2 protocol) to play the stream URL.

> **Latency note:** this HTTP/HLS approach — the same one tools like mkchromecast use — has an inherent delay of a few seconds. It is great for presentations, photos, and videos; it is not suitable for gaming.

## Requirements

- GNOME Shell **48–50** (Wayland or X11; capture uses the ScreenCast portal)
- PipeWire + `xdg-desktop-portal-gnome` (default on any modern GNOME distro)
- GStreamer 1.x with plugins: `base`, `good`, `bad`, `ugly` (for `x264enc`) and `libav` (for AAC encoding)
- `pactl` (pulseaudio-utils) for locating the system-audio monitor device
- Rust toolchain (only to build the daemon)

Debian/Ubuntu build + runtime packages:

```sh
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
    gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly \
    gstreamer1.0-libav gstreamer1.0-pipewire pulseaudio-utils cargo
```

Fedora:

```sh
sudo dnf install gstreamer1-devel gstreamer1-plugins-base-devel \
    gstreamer1-plugins-good gstreamer1-plugins-bad-free gstreamer1-plugins-ugly \
    gstreamer1-libav pipewire-gstreamer pulseaudio-utils cargo
```

## Install

```sh
make install-local
```

This builds the daemon (`cargo build --release`), installs:

- the extension to `~/.local/share/gnome-shell/extensions/gnome-shell-cast@oxygenws.com`
- the daemon binary to `~/.local/bin/gnome-shell-cast-daemon`
- the D-Bus activation file to `~/.local/share/dbus-1/services/`

Then log out and back in (Wayland), and enable the extension:

```sh
gnome-extensions enable gnome-shell-cast@oxygenws.com
```

## Usage

1. Click the cast icon in the top panel.
2. Pick a Chromecast from the discovered device list.
3. Choose **Cast Screen** or **Cast Window** — GNOME's picker dialog opens for the source.
4. To end, click the icon and choose **Stop Casting**.

Preferences (resolution cap, framerate, bitrate) are under the ⚙ menu entry or `gnome-extensions prefs gnome-shell-cast@oxygenws.com`.

## Troubleshooting

```sh
# Extension logs
journalctl -f -o cat /usr/bin/gnome-shell

# Daemon logs
journalctl --user -f | grep gnome-shell-cast
# or run it by hand with verbose logging:
RUST_LOG=debug ~/.local/bin/gnome-shell-cast-daemon
```

- **No devices found:** the Chromecast must be on the same network/VLAN, and mDNS (UDP 5353) must not be blocked.
- **No audio:** system audio is captured from the default sink's monitor via `pactl get-default-sink`. Check `pactl` is installed and audio isn't going to a different sink.
- **Playback fails on the TV:** confirm `gst-inspect-1.0 x264enc hlssink2` finds both elements.

## Manual test plan

1. `make install-local`, re-login, enable the extension.
2. Panel shows the cast icon; menu lists your Chromecast within ~5 s.
3. *Cast Screen* → portal picker → picture appears on the TV in a few seconds, with system audio.
4. *Cast Window* → portal shows only windows; only that window is streamed.
5. *Stop Casting* → TV returns to the ambient screen; daemon exits after ~10 min idle.

## Known limitations (v1)

- A few seconds of latency (inherent to HTTP/HLS casting).
- Sender and Chromecast must be on the same LAN.
- DRM-protected content will be black in the capture.
- Audio is the full system mix (no per-app capture).
- Not yet published to extensions.gnome.org; no prebuilt daemon binaries.

## License

[MIT](LICENSE)
