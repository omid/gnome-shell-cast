---
name: debug-cast
description: Diagnose a failing or misbehaving cast - black screen, picture breaking up, sessions dropping, falling back to HLS, or devices not found. Runs the daemon with debug logging, drives a cast from the CLI, and reads the signals that distinguish sender-side from receiver-side faults.
---

# Debugging a cast

Read the logs before theorising. Every bug in this project so far looked like
something other than what it was.

## Run the daemon with debug logging

```sh
pkill -f '^/home/omid/.local/bin/gnome-shell-cast-daemon'
setsid env RUST_LOG=debug ~/.local/bin/gnome-shell-cast-daemon > /tmp/cast-debug.log 2>&1 < /dev/null &
```

Do **not** truncate that file while the daemon holds it open — the write offset
is kept, so the file fills with NULs. Use a fresh path per run.

## Drive a cast without touching the UI

```sh
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 ListDevices
# source: 0 screen, 1 window, 2 audio, 3 choose (portal picker)
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 \
    StartCast "sua{sv}" "<device-id>" 0 0
busctl --user call org.gnome.ShellCast /org/gnome/ShellCast org.gnome.ShellCast1 StopCast
```

This casts to a real device in the room. Say what you are about to do, and stop
the cast when finished. `source 3` opens a portal dialog that waits for a click.

## What to grep for

```sh
grep -aE "INFO|WARN" /tmp/cast-debug.log | grep -vE "discovery|mirror pipeline"
grep -ac "dropping a packet" /tmp/cast-debug.log
grep -ac "picture loss" /tmp/cast-debug.log
```

| Line | Meaning |
|---|---|
| `receiver ANSWER: udp port …, streams […]` | negotiation succeeded |
| `sending first video frame to the receiver` | the encoder is producing |
| `receiver acknowledged video up to frame N` | the receiver is **receiving** RTP |
| `no RTCP feedback from the receiver after 3s` | it is **not** receiving; suspect network/address |
| `the network would not take N packet(s)` | the link cannot take the bitrate |
| `mirroring unavailable, falling back to HLS: …` | read the reason, it is usually specific |

Frames flowing plus ACKs arriving means the sender side is healthy and the
problem is on the receiver or the TV (overscan, wedged receiver app).

## Confirm capture is alive independently of the daemon

```sh
pw-dump | python3 -c "
import json,sys
for o in json.load(sys.stdin):
    if o.get('type')=='PipeWire:Interface:Link':
        i=o['info']; print('link',i['output-node-id'],'->',i['input-node-id'],i.get('state'))"
```

An `active` link from the gnome-shell node to the daemon node means the portal
capture is genuinely feeding frames. Daemon CPU in the tens of percent means the
encoder is working — but note `pipewiresrc resend-last=true` re-encodes a stale
frame, so CPU alone does not prove live content.

## Receiver-side gotchas

- **Repeated rapid start/stop wedges the Chrome mirroring app**: it will accept
  `LAUNCH` and never send an ANSWER. Wait, or power-cycle the device.
- **A picture cropped on all four sides is TV overscan**, not the cast. See
  `TROUBLESHOOTING.md`.
- **After a resume, the first mDNS resolve often carries only the AAAA record.**
  On an IPv4-only network that address is unusable; discovery re-picks once the
  A record arrives.
