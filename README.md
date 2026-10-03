# GNOME Shell Cast

Cast your **whole screen or a single window** to a **Chromecast** device, or send your **system audio** to a Chromecast speaker, smart display, or cast group, right from the GNOME Shell top panel.

The project has two parts, shipped together in this repository:

| Component | Language | Role |
|---|---|---|
| `extension/` | GJS | Panel indicator: device list, *Cast Screen* / *Cast Window*, *Stop Casting*, preferences |
| `daemon/` | Rust | Does the heavy lifting: Chromecast discovery (mDNS) and control (CASTv2), screen/window capture (XDG ScreenCast portal → PipeWire), encoding (GStreamer, H.264 + AAC), and serving the stream over HTTP (live HLS for screens; a progressive MP3/AAC stream for audio-only receivers) |

The two talk over the D-Bus session bus (`org.gnome.ShellCast1`). The daemon is D-Bus activatable, so it starts on demand and exits when idle.

## Screenshots

<p align="center">
  <img src="assets/Screenshot1.png" alt="Preferences dialog and the quick settings menu while casting" width="70%">
  <br>
  <em>Preferences and the quick settings menu while casting</em>
</p>

<p align="center">
  <img src="assets/Screenshot2.png" alt="Top-bar tray menu while casting" width="70%">
  <br>
  <em>Top-bar tray menu while casting</em>
</p>

## How it works

Chromecast's Default Media Receiver plays media it pulls over HTTP. When you start a cast:

1. The daemon opens an XDG ScreenCast portal session - GNOME shows its native picker for a monitor or a window.
2. A GStreamer pipeline captures the PipeWire stream, encodes H.264 video and AAC system audio, and writes a live HLS stream into `$XDG_RUNTIME_DIR/gnome-shell-cast/`.
3. A tiny built-in HTTP server serves that stream on your LAN.
4. The daemon tells the Chromecast (CASTv2 protocol) to play the stream URL.

> **Latency note:** this HTTP/HLS approach - the same one tools like mkchromecast use - has an inherent delay of a few seconds. It is great for presentations, photos, and videos; it is not suitable for gaming.

**Audio-only receivers** (Chromecast Audio, Google/Nest speakers and smart displays, cast groups) advertise no video, and their Default Media Receiver rejects the live HLS stream. For them the daemon skips screen capture entirely and streams your **system audio** as a continuous MP3 (falling back to AAC) over HTTP, the way an internet-radio station does. Pick such a device from the menu and it offers a single **Cast audio** action.

## Requirements

> Missing a plugin? See
> **[TROUBLESHOOTING.md](TROUBLESHOOTING.md#dependencies)** for per-distro
> package lists (Debian/Ubuntu, Fedora, Arch, openSUSE) and a symptom-to-fix
> table.

- GNOME Shell **48–50** (Wayland or X11; capture uses the ScreenCast portal)
- PipeWire + `xdg-desktop-portal-gnome` (default on any modern GNOME distro)
- GStreamer 1.x with plugins: `base`, `good`, `bad`, `ugly` (for `x264enc`) and `libav` (for AAC encoding). Audio-only casts use `lamemp3enc` (from `good`) with AAC as a fallback
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

## Installing the daemon

If you got the **extension** from extensions.gnome.org, install the **daemon**
separately (it can't be shipped there). The extension makes this easy: until the
daemon is present, its menu shows a **“Set up the cast daemon”** item that opens a
dialog with a ready-to-copy command. It downloads a checksum-verified binary for
your CPU from the project's GitHub Releases into `~/.local/bin` - **nothing runs
as root**:

```sh
curl -fsSL https://raw.githubusercontent.com/omid/gnome-shell-cast/refs/heads/main/scripts/install.sh | sh
```

(The dialog fills in the version matching your installed extension.) You can
inspect [`scripts/install.sh`](scripts/install.sh) before running it.

**Updating:** when the extension updates to a version that needs a newer daemon,
its menu shows **“Update the cast daemon (v… → v…)”** - run the same command from
that dialog and it installs the matching release.

**Uninstall:**

```sh
rm -f ~/.local/bin/gnome-shell-cast-daemon \
      ~/.local/share/dbus-1/services/org.gnome.ShellCast.service
```

## Install from source

To build and install both halves from this repository:

```sh
make install
```

This builds the daemon (`cargo build --release`) and installs:

- the extension to `~/.local/share/gnome-shell/extensions/gnome-shell-cast@oxygenws.com`
- the daemon binary to `~/.local/bin/gnome-shell-cast-daemon`
- the D-Bus activation file to `~/.local/share/dbus-1/services/`

Then log out and back in (Wayland), and enable the extension:

```sh
gnome-extensions enable gnome-shell-cast@oxygenws.com
```

(`make install-daemon` installs only the daemon half - useful on a machine that
already has the extension from extensions.gnome.org but no supported prebuilt
binary.)

### NixOS

Neither the prebuilt binary nor `make install` works on NixOS, so the repo ships
a flake instead. It builds both halves from the same revision and bundles the
GStreamer plugins and `pactl` into the daemon, so the session needs no setup.

```nix
inputs.gnome-shell-cast.url = "github:omid/gnome-shell-cast";
```

See [nix/README.md](nix/README.md) for installing it, running it without
changing your configuration, and the firewall and portal settings to check.

## Usage

1. Click the cast icon in the top panel.
2. Pick a Chromecast from the discovered device list.
3. Choose **Cast screen** or **Cast window** - GNOME's picker dialog opens for the source.
   Audio-only devices (speakers, cast groups) show a single **Cast audio** action instead.
4. To end, click the icon and choose **Stop casting**.

Preferences (resolution cap, framerate, bitrate, codec and encoder) are under
the ⚙ menu entry or
`gnome-extensions prefs gnome-shell-cast@oxygenws.com`. Leave the codec on
*Automatic* normally.

## Troubleshooting

See **[TROUBLESHOOTING.md](TROUBLESHOOTING.md)** for the full list.

## Manual test plan

1. `make install`, re-login, enable the extension.
2. Panel shows the cast icon; menu lists your Chromecast within ~5 s.
3. *Cast Screen* → portal picker → picture appears on the TV in a few seconds, with system audio.
4. *Cast Window* → portal shows only windows; only that window is streamed.
5. *Stop Casting* → TV returns to the ambient screen; daemon exits after ~10 min idle.

## Known limitations

- A few seconds of latency on the HTTP/HLS fallback (low-latency Cast Streaming
  is used when the receiver supports it).
- Resolutions above 1080p realistically need a hardware encoder (VA-API or
  NVENC); software encoding above 1080p will not keep up in realtime. Raise the
  bitrate to match the resolution — the default 4000 kbit/s suits 720p.
- With resolution set to *Native*, Cast Streaming advertises 1080p to the
  receiver regardless of the captured size.
- Sender and Chromecast must be on the same LAN.
- DRM-protected content will be black in the capture.
- Audio is the full system mix (no per-app capture).

## License

[MIT](LICENSE).

The Cast Streaming protocol code in `daemon/src/streaming/openscreen/` is a port
of Chromium's [openscreen](https://chromium.googlesource.com/openscreen)
`cast/streaming` and is used under its BSD-3-Clause licence — see
[NOTICE](NOTICE) and
[`daemon/src/streaming/openscreen/NOTICE`](daemon/src/streaming/openscreen/NOTICE)
for the required attribution. "Google Cast" and "Chromecast" are trademarks of
Google LLC; this project is not affiliated with Google.
