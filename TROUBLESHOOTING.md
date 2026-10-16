# Troubleshooting

Problems seen in the wild, what causes them, and how to fix them. The daemon
uses **GStreamer** to encode and **PipeWire** to capture, so a missing plugin or
package is the single most common cause of a cast that will not start — start
with [Dependencies](#dependencies) if you have never had a cast work.

If none of this helps, please
[open an issue](https://github.com/omid/gnome-shell-cast/issues) and include the
logs from [Collecting logs](#collecting-logs).

| Topic | What's in it |
|---|---|
| [Collecting logs](#collecting-logs) | Daemon, extension, and verbose-run log commands |
| [Dependencies](#dependencies) | What GStreamer/PipeWire pieces are needed, per-distro package lists, and which package fixes which symptom |
| [Picture](#picture) | Overscan cropping, black screen or break-up, the Chromecast backdrop |
| [Connection](#connection) | Unreachable routes, the HLS fallback, no devices found, broken pipe on exit |
| [Audio](#audio) | Silent casts, audio-only receivers, checking for encoders |
| [Extension](#extension) | A stale panel icon, `INACTIVE` state, a lingering screen-sharing indicator |
| [Developing](#developing) | Why a JS or daemon change appears to do nothing |

## Collecting logs

```sh
# Daemon logs (every line is tagged "gnome-shell-cast")
journalctl --user -f -g gnome-shell-cast

# Extension logs
journalctl -f -o cat /usr/bin/gnome-shell

# Verbose daemon run: stop the running one first, then start it by hand
pkill -f '^\$HOME/.local/bin/gnome-shell-cast-daemon'
RUST_LOG=debug ~/.local/bin/gnome-shell-cast-daemon
```

`RUST_LOG=debug` is worth the noise — RTP/RTCP problems are invisible at the
default level.

---

## Dependencies

> The daemon binary from the [Releases](https://github.com/omid/gnome-shell-cast/releases)
> is dynamically linked, so these libraries must be present at runtime even if
> you didn't build from source.

### What's needed

**Runtime (to cast):**

- GStreamer 1.x core and the **base**, **good**, **bad**, and **ugly** plugin sets
  - `x264enc` (H.264, from *ugly*) - the HLS fallback and a hardware-free H.264 path
  - `vp8enc` / `vp9enc` (VP8/VP9, from *good*/*vpx*) - Cast Streaming (mirroring)
  - `av1enc` (aom) or `svtav1enc` (SVT-AV1) - optional AV1 mirroring
  - an AAC encoder: `fdkaacenc` (*bad*), `avenc_aac` (*libav*), or `faac`
  - `lamemp3enc` (MP3, from *good*) - preferred for audio-only casts (speakers, smart displays, cast groups); the AAC encoder above is used if it is missing
  - `opusenc` (Opus audio, from *good*)
- **PipeWire** and its GStreamer plugin (`pipewiresrc`), plus `xdg-desktop-portal-gnome`
- `pactl` (for locating the system-audio monitor)
- Optional, for hardware encoding: a GStreamer plugin *and* a driver, and which
  pair you need depends on the graphics card - see
  [Hardware encoding by graphics card](#hardware-encoding-by-graphics-card).

**Build only (if compiling the daemon yourself):** the Rust toolchain and the
GStreamer development headers.

### Install by distribution

#### Debian / Ubuntu

```sh
sudo apt install \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
    gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav \
    gstreamer1.0-pipewire pipewire pulseaudio-utils
# hardware encoding: the va plugin is in plugins-bad above; add the driver:
#   va-driver-all
# building from source: cargo libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev
```

#### Fedora

```sh
sudo dnf install \
    gstreamer1-plugins-base gstreamer1-plugins-good \
    gstreamer1-plugins-bad-free gstreamer1-plugins-ugly gstreamer1-libav \
    pipewire-gstreamer pipewire-utils pulseaudio-utils
# hardware encoding: the va plugin is in plugins-bad-free above; add the driver:
#   intel-media-driver (Intel) or mesa-va-drivers (AMD)
# building from source: cargo gstreamer1-devel gstreamer1-plugins-base-devel
```

(For `x264enc`/`faac` and other patent-encumbered encoders, Fedora users
typically enable [RPM Fusion](https://rpmfusion.org/).)

#### Arch Linux

```sh
sudo pacman -S \
    gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly \
    gst-libav gst-plugin-pipewire pipewire-pulse libpulse
# AV1: aom or svt-av1
# hardware encoding: gst-plugin-va (a separate package here), plus the driver:
#   intel-media-driver (Intel) or libva-mesa-driver (AMD)
# building from source: rust
```

#### NixOS

The flake builds both halves and bakes the GStreamer plugins into the daemon, so
nothing has to be installed alongside it - see [nix/README.md](nix/README.md).
The **va** plugin is among those bundled plugins, so only the VA-API driver is
yours to add:

```nix
# Intel; AMD is covered by Mesa, which is already there
hardware.graphics.extraPackages = with pkgs; [ intel-media-driver ];
```

#### openSUSE

```sh
sudo zypper install \
    gstreamer-plugins-base gstreamer-plugins-good \
    gstreamer-plugins-bad gstreamer-plugins-ugly gstreamer-plugins-libav \
    gstreamer-plugin-pipewire pipewire-tools pulseaudio-utils
# hardware encoding: the va plugin is in gstreamer-plugins-bad above; add the
#   driver: intel-media-driver (Intel) or Mesa's VA-API driver (AMD)
# building from source: cargo gstreamer-devel gstreamer-plugins-base-devel
```

### Hardware encoding by graphics card

Software encoding always works, so none of this is required - it lowers CPU use
and is what makes 1440p and 4K realistic. Preferences (Advanced -> Encoding)
says which of these your machine is missing, and the cast details line in the
menu names the encoder actually in use.

| Card | Path | GStreamer plugin | Driver |
|---|---|---|---|
| Intel, Gen8 and later (`i915`, `xe`) | VA-API | **va** (`vah264enc`, `vah264lpenc`, `vavp9lpenc`) | `intel-media-driver` |
| Intel, older Gen (`i915`) | VA-API, H.264 only | **va** | the legacy `libva-intel-driver` (i965) |
| AMD GCN and later (`amdgpu`) | VA-API | **va** | Mesa's VA-API driver |
| AMD pre-GCN (`radeon`) | usually none | - | Mesa's, but these cards rarely encode |
| NVIDIA, proprietary or open kernel module | NVENC | **nvcodec** (`nvh264enc`, `nvav1enc`) | the NVIDIA driver, including its NVENC library |
| NVIDIA on nouveau | none | - | nouveau exposes no encoder; the proprietary driver does |
| Arm SoCs - Raspberry Pi 4 (`v3d`/`vc4`), Rockchip, Mali (`panfrost`/`panthor`), Qualcomm (`msm`) | V4L2 | **good** (`v4l2h264enc`, from the kernel's own encoder device) | in the kernel; nothing to install |
| Virtual GPUs - `virtio_gpu`, `vmwgfx`, `qxl` - and server display chips (`ast`, `mgag200`) | none | - | no encoder exists to reach |

V4L2 encoders come from the kernel, so the elements exist only where a device
advertises that codec - `v4l2h264enc` on a Raspberry Pi 4, nothing at all on a
Raspberry Pi 5, whose hardware H.264 encoder was removed. There is no package to
install for them: either the kernel exposes the device or it does not.

The **va** plugin covers Intel and AMD together; it is a separate package only
on some distributions (see [above](#install-by-distribution)), and elsewhere it
sits in the *bad* set this daemon already needs. **nvcodec** ships in that same
*bad* set everywhere, so for NVIDIA the missing piece is nearly always the
driver, not the plugin.

Two things worth knowing before chasing a driver:

- **The driver decides which encoders exist**, not the plugin. Intel encodes VP9
  only in low power mode, so the element is `vavp9lpenc`; several Intel
  generations have no VP8 or AV1 encoder at all. If the receiver picks a codec
  your card cannot encode, that cast runs in software even though hardware
  encoding "works".
- **A discrete GPU that sleeps can come and go.** With runtime power management
  the NVENC elements may be absent while the card is powered down, so the same
  machine can report hardware encoding as available or not between two daemon
  starts. Every hardware candidate is opened before it is used, so one that is
  registered but cannot start is skipped rather than failing the cast.
- **On a machine with two hardware paths, automatic prefers VA-API.** Set *Video
  encoder* in preferences to `VA-API`, `NVENC` or `V4L2` to pin the other one -
  useful on a hybrid laptop whose integrated encoder is the weaker of the two.

### Which package fixes which symptom

| Symptom | Likely cause | Install |
|---|---|---|
| No devices ever appear in the menu | not a library issue - mDNS (UDP 5353) blocked, or the device is on another subnet/VLAN (see [No devices found](#no-devices-found)) | - |
| Cast starts then fails; log says *"no video encoder is installed"* | no VP8/VP9/etc. encoder | plugins **good** (vpx) and **ugly** (x264) |
| Casting always uses HLS (multi-second lag), never low-latency | mirroring encoders missing, so it falls back | plugins **good** (`vp8enc`) |
| Log: *"no AAC encoder found"* | no AAC encoder for the HLS fallback | plugins **bad** (`fdkaacenc`) or **libav** |
| Video works but there's no audio | `pactl` missing, or no monitor source | `pulseaudio-utils` / `pipewire-pulse` |
| Casting to a speaker or cast group plays nothing | audio-only receivers need an MP3 or AAC encoder | plugins **good** (`lamemp3enc`) or **bad** (`fdkaacenc`) |
| Log: *"parsing the mirroring pipeline"* fails | GStreamer base/good plugins incomplete | plugins **base** + **good** |
| Details line never shows hardware, and preferences says hardware encoding is unavailable | the plugin or the driver for your graphics card is missing, so software encoding is used (which still works) | see [Hardware encoding by graphics card](#hardware-encoding-by-graphics-card) |
| Screen picker never opens | portal missing | `xdg-desktop-portal-gnome` + PipeWire |

Check which encoders GStreamer can see:

```sh
gst-inspect-1.0 vp8enc x264enc opusenc pipewiresrc   # should all print details
gst-inspect-1.0 | grep -iE 'vah264enc|nvh264enc'     # hardware H.264, if any
```

---

## Picture

### The picture is cut off on all four sides

Your TV is applying **overscan**, an old habit from analogue broadcast where the
panel zooms ~5 % and throws away the edges. Nothing is wrong with the cast: the
whole desktop is sent, scaled to the exact resolution you picked, with no
cropping on our side.

Fix it in the TV's picture settings. The option is per-input, so set it on the
input your Chromecast device is connected to:

| Vendor | Setting |
|---|---|
| Samsung, LG | *Just Scan* |
| Sony | *Screen Fit* or *Full Pixel* |
| Panasonic, Philips | *Unscaled*, *1:1*, or *Full* |

If your TV genuinely has no such setting, open an issue — we can add optional
overscan compensation (scaling the desktop down and padding it with black so
the TV's zoom eats the padding instead of your windows).

### Black screen, or the picture breaks up

Look for this in the daemon log:

```
WARN the network would not take N packet(s) of a frame; the picture will break
     up - try a lower bitrate
```

A key frame is a burst of 50–100 UDP packets. If the link cannot absorb the
burst, the receiver never gets a complete key frame and shows black. Fixes, in
order of effectiveness:

1. **Lower the bitrate** in preferences (the default 4000 kbit/s suits 720p).
2. **Lower the resolution or framerate.**
3. **Improve the Wi-Fi link** — check your ping to the device. On a LAN it
   should be single-digit milliseconds; 90–170 ms means a weak link, and casting
   will struggle no matter what you set.

### The TV shows the Chromecast logo (backdrop) instead of your screen

The receiver app exited or never started rendering. Check the log for
`device ended the mirroring session` or a fallback to HLS. See
[The cast falls back to HLS](#the-cast-falls-back-to-hls-or-the-receiver-never-answers)
below.

---

## Connection

### `probing route to device: Network is unreachable (os error 101)`

The device was resolved to an address your machine has no route to — almost
always an **IPv6 address on an IPv4-only network**. A single mDNS announcement
can carry only part of the record, and after a suspend/resume the AAAA record
often arrives minutes before the A record.

Check whether you have IPv6 at all:

```sh
ip -6 addr show scope global      # no output = no global IPv6
ip -6 route get <device-ipv6>     # "Network is unreachable" confirms it
```

Recent versions pick a *reachable* address, remember every address a device has
announced, and refuse to downgrade a working address to an unreachable one — so
this should resolve itself. If you hit it on an older build, restart the daemon
once the device has been announced fully:

```sh
pkill -f '^\$HOME/.local/bin/gnome-shell-cast-daemon'
```

### The cast falls back to HLS, or the receiver never answers

```
WARN mirroring unavailable, falling back to HLS: timed out waiting for the
     receiver's ANSWER
```

The mirroring app launched but never completed negotiation. The usual cause is
the receiver being **wedged after repeated rapid start/stop cycles** — common
while testing. Fixes:

1. Wait a minute and try again.
2. Power-cycle the Chromecast device, or force-stop the cast app on it.

HLS still works in this state; it just has seconds of latency instead of
sub-second. A cast that *always* falls back is usually a missing mirroring
encoder instead — see [Dependencies](#dependencies).

### No devices found

- The device must be on the same network/VLAN as your machine.
- mDNS (UDP 5353) must not be blocked — client isolation on guest Wi-Fi
  networks will do exactly this.
- Give it ~5 seconds after opening the menu; discovery is asynchronous.

### The daemon warns about a broken pipe and exits

```
WARN failed to emit signal: I/O error: Broken pipe (os error 32)
WARN D-Bus connection lost, exiting
```

Normal. The session bus went away (usually a logout), so the daemon shuts down
and D-Bus activation starts a fresh one when it is next needed.

---

## Audio

### No audio

System audio is captured from the default sink's monitor via
`pactl get-default-sink`. Check that `pactl` is installed, and that audio is
actually going to the sink you expect rather than a different one.

### Audio-only receivers reject the stream

Speakers, smart displays, and cast groups advertise no video and their Default
Media Receiver rejects live HLS. The daemon detects this and streams system
audio as MP3/AAC instead, offering a single **Cast audio** action. If that fails,
you are probably missing an AAC encoder — see below.

### Missing GStreamer plugins

```sh
gst-inspect-1.0 x264enc hlssink2     # HLS fallback path
gst-inspect-1.0 vp9enc               # Cast Streaming (mirroring) path
gst-inspect-1.0 fdkaacenc            # AAC for audio-only receivers
```

Anything not found needs its plugin package installed. The daemon tries several
AAC encoders in turn (`fdkaacenc`, `avenc_aac`, `voaacenc`, `faac`), so missing
`gst-libav` alone is not fatal. See [Dependencies](#dependencies) for package
names.

---

## Extension

### The panel icon or toggle does not update while casting

Fixed in recent versions. If you see the menu only catching up when you open it,
you are on an older build — update and log out and back in.

### The extension shows as INACTIVE

```
$ gnome-extensions info gnome-shell-cast@oxygenws.com
  Enabled: Yes
  State: INACTIVE
```

Expected **while the screen is locked**: the extension does not declare the
`unlock-dialog` session mode, so GNOME disables it on the lock screen. It comes
back when you unlock. Confirm with:

```sh
loginctl show-session $(loginctl list-sessions --no-legend | awk 'NR==1{print $1}') -p LockedHint
```

If it is `LockedHint=no` and still inactive, check the shell log for an
exception thrown during enable.

### GNOME's "screen is being shared" indicator stays on

The portal session was not closed. Recent versions close it explicitly and wait
for the compositor to confirm. If it lingers, stopping the cast or letting the
daemon exit (~10 minutes idle) clears it.

---

## Developing

### Changes to the extension's JavaScript have no effect

GNOME Shell caches an extension's ES modules for the life of the process, so
`gnome-extensions disable/enable` does **not** reload changed JS. On X11 you can
restart the shell with <kbd>Alt</kbd>+<kbd>F2</kbd> → `r`; on **Wayland you must
log out and back in**.

`stylesheet.css` is the exception — the shell loads and unloads it on
enable/disable, so pure CSS changes apply with a quick disable/enable. Worth
keeping visual tweaks in CSS while iterating.

### Changes to the daemon have no effect

The old process keeps running after `make install-daemon`. Kill it, and if your
session uses dbus-broker, reload it so a new service file is picked up:

```sh
make install-daemon
systemctl --user reload dbus-broker.service
pkill -f '^\$HOME/.local/bin/gnome-shell-cast-daemon'
```

Anchor the `pkill` pattern with `^` — an unanchored `-f gnome-shell-cast-daemon`
also matches the shell you are typing in.

Verify which build is installed:

```sh
md5sum daemon/target/release/gnome-shell-cast-daemon ~/.local/bin/gnome-shell-cast-daemon
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 GetVersion
```
