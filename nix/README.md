<!-- Kept next to the package definitions it describes; `flake.nix` stays at
the repository root because that is where Nix looks for it. -->

# GNOME Shell Cast on NixOS

Neither the prebuilt binary nor `make install` works on NixOS, so the repo ships
a flake instead. It builds both halves from the same revision (the extension
requires an exactly matching daemon version) and bakes the GStreamer plugins and
`pactl` into the daemon's wrapper, so nothing has to be added to your session
environment.

```nix
{
  inputs.gnome-shell-cast.url = "github:omid/gnome-shell-cast";

  # in your configuration.nix / a module:
  environment.systemPackages = [
    inputs.gnome-shell-cast.packages.${pkgs.system}.default
  ];
}
```

That single package provides the daemon, the extension, and the D-Bus activation
file. `services.dbus.packages` already includes the system path, so activation
needs no extra wiring - just enable the extension after a re-login:

```sh
gnome-extensions enable gnome-shell-cast@oxygenws.com
```

To try it without touching your configuration:

```sh
nix run github:omid/gnome-shell-cast#daemon
```

`packages.daemon` and `packages.extension` are available separately, for a
machine that already has the extension from extensions.gnome.org.

Two things to check if casting does not work:

- **Firewall.** Discovery needs multicast DNS, and the Chromecast pulls the
  stream back over HTTP from an *ephemeral* port, so there is no single port to
  open. Trust your LAN interface
  (`networking.firewall.trustedInterfaces = [ "..." ]`), or open
  `networking.firewall.allowedUDPPorts = [ 5353 ]` plus a wide
  `allowedTCPPortRanges`.
- **Portal.** Capture goes through the ScreenCast portal, so PipeWire and
  `xdg-desktop-portal-gnome` must be present - both are on by default with
  `services.desktopManager.gnome.enable = true`.

Hardware encoding needs one thing the wrapper cannot provide: the VA-API driver
for the graphics card, which is what decides whether the bundled **va** plugin
registers any encoder at all. Without it the daemon encodes in software, which
still casts - preferences says so, and the cast details line names the encoder.

```nix
# Intel; AMD is covered by Mesa, which is already there
hardware.graphics.extraPackages = with pkgs; [ intel-media-driver ];
```

`nix develop` gives a shell with the Rust toolchain, GStreamer headers, and the
tooling the `Makefile` targets expect.
