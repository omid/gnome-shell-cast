# Dependencies & troubleshooting

GNOME Shell Cast's daemon uses **GStreamer** to encode and **PipeWire** to
capture. If casting fails, a device never appears, or you have no audio, a
missing GStreamer plugin or PipeWire package is almost always the cause. This
page lists what to install per distribution and maps common symptoms to the
package that fixes them.

> The daemon binary from the [Releases](https://github.com/omid/gnome-shell-cast/releases)
> is dynamically linked, so these libraries must be present at runtime even if
> you didn't build from source.

## What's needed

**Runtime (to cast):**

- GStreamer 1.x core and the **base**, **good**, **bad**, and **ugly** plugin sets
  - `x264enc` (H.264, from *ugly*) - the HLS fallback and a hardware-free H.264 path
  - `vp8enc` / `vp9enc` (VP8/VP9, from *good*/*vpx*) - Cast Streaming (mirroring)
  - `av1enc` (aom) or `svtav1enc` (SVT-AV1) - optional AV1 mirroring
  - an AAC encoder: `fdkaacenc` (*bad*), `avenc_aac` (*libav*), or `faac`
  - `opusenc` (Opus audio, from *good*)
- **PipeWire** and its GStreamer plugin (`pipewiresrc`), plus `xdg-desktop-portal-gnome`
- `pactl` (for locating the system-audio monitor)
- Optional, for hardware encoding: the GStreamer **VA-API** plugin (`vah264enc`, …)
  or NVIDIA **nvcodec** plugin (`nvh264enc`, …)

**Build only (if compiling the daemon yourself):** the Rust toolchain and the
GStreamer development headers.

## Install by distribution

### Debian / Ubuntu

```sh
sudo apt install \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
    gstreamer1.0-pipewire pipewire pulseaudio-utils
# hardware encoding (Intel/AMD): gstreamer1.0-vaapi
# building from source: cargo libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
```

### Fedora

```sh
sudo dnf install \
    gstreamer1-plugins-base gstreamer1-plugins-good \
    gstreamer1-plugins-bad-free gstreamer1-plugins-ugly gstreamer1-libav \
    pipewire-gstreamer pipewire-utils pulseaudio-utils
# building from source: cargo gstreamer1-devel gstreamer1-plugins-base-devel
```

(For `x264enc`/`faac` and other patent-encumbered encoders, Fedora users
typically enable [RPM Fusion](https://rpmfusion.org/).)

### Arch Linux

```sh
sudo pacman -S \
    gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly \
    gst-libav gst-plugin-pipewire pipewire-pulse libpulse
# AV1: aom or svt-av1 ;  hardware encoding: gstreamer-vaapi
# building from source: rust
```

### openSUSE

```sh
sudo zypper install \
    gstreamer-plugins-base gstreamer-plugins-good \
    gstreamer-plugins-bad gstreamer-plugins-ugly gstreamer-plugins-libav \
    gstreamer-plugin-pipewire pipewire-tools pulseaudio-utils
# building from source: cargo gstreamer-devel gstreamer-plugins-base-devel
```

## Symptom → fix

| Symptom | Likely cause | Install |
|---|---|---|
| No devices ever appear in the menu | not a library issue - mDNS (UDP 5353) blocked, or the device is on another subnet/VLAN | - |
| Cast starts then fails; log says *"no video encoder is installed"* | no VP8/VP9/etc. encoder | plugins **good** (vpx) and **ugly** (x264) |
| Casting always uses HLS (multi-second lag), never low-latency | mirroring encoders missing, so it falls back | plugins **good** (`vp8enc`) |
| Log: *"no AAC encoder found"* | no AAC encoder for the HLS fallback | plugins **bad** (`fdkaacenc`) or **libav** |
| Video works but there's no audio | `pactl` missing, or no monitor source | `pulseaudio-utils` / `pipewire-pulse` |
| Log: *"parsing the mirroring pipeline"* fails | GStreamer base/good plugins incomplete | plugins **base** + **good** |
| Details line never shows hardware | no VA-API/NVENC GStreamer plugin (software encoding is used, which still works) | `gstreamer-vaapi` (Intel/AMD) |
| Screen picker never opens | portal missing | `xdg-desktop-portal-gnome` + PipeWire |

Check which encoders GStreamer can see:

```sh
gst-inspect-1.0 vp8enc x264enc opusenc pipewiresrc   # should all print details
gst-inspect-1.0 | grep -iE 'vah264enc|nvh264enc'     # hardware H.264, if any
```

Still stuck? Open an issue at
<https://github.com/omid/gnome-shell-cast/issues> with your distro and the
output of `journalctl --user -g gnome_shell_cast` from a failed cast.
