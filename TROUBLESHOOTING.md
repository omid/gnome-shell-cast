# Troubleshooting

Problems seen in the wild, what causes them, and how to fix them. If none of
this helps, please [open an issue](https://github.com/omid/gnome-shell-cast/issues)
and include the logs from [Collecting logs](#collecting-logs).

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
sub-second.

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
`gst-libav` alone is not fatal. See
[docs/DEPENDENCIES.md](docs/DEPENDENCIES.md) for package names.

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
