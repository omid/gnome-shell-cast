{
  description = "Cast the GNOME desktop to Chromecast devices - GNOME Shell extension and its D-Bus daemon";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      overlays.default = final: _prev: {
        gnome-shell-cast-daemon = final.callPackage ./nix/daemon.nix { };
        gnome-shell-extension-gnome-shell-cast = final.callPackage ./nix/extension.nix { };
      };

      packages = forAllSystems (
        pkgs:
        let
          daemon = pkgs.callPackage ./nix/daemon.nix { };
          extension = pkgs.callPackage ./nix/extension.nix { };
        in
        {
          inherit daemon extension;

          # Both halves in one path: bin/, the extension directory, and the
          # D-Bus activation file. This is what belongs in systemPackages.
          default = pkgs.symlinkJoin {
            name = "gnome-shell-cast-${daemon.version}";
            paths = [
              daemon
              extension
            ];
            meta = daemon.meta // {
              description = "Cast your screen or a single window to a Chromecast from the GNOME top panel";
            };
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            pkg-config
            cmake
            perl
            glib
            gst_all_1.gstreamer
            gst_all_1.gst-plugins-base
            gettext
            gnumake
            nodejs
            jq
            gnome-shell # gnome-extensions, for `make zip` / `make shexli`
          ];
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
