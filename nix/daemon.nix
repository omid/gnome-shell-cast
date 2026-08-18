{
  lib,
  rustPlatform,
  makeWrapper,
  pkg-config,
  cmake,
  perl,
  glib,
  gst_all_1,
  pipewire,
  pulseaudio,
}:

let
  cargoToml = lib.importTOML ../daemon/Cargo.toml;

  gstPlugins = [
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    gst_all_1.gst-plugins-bad
    gst_all_1.gst-plugins-ugly
    gst_all_1.gst-libav
    pipewire # pipewiresrc ships here, not in any gst-plugins-*
  ];
in
rustPlatform.buildRustPackage {
  pname = "gnome-shell-cast-daemon";
  version = cargoToml.package.version;

  src = lib.cleanSource ../.;

  cargoRoot = "daemon";
  buildAndTestSubdir = "daemon";
  cargoLock.lockFile = ../daemon/Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
    cmake # aws-lc-sys, via rustls' default aws-lc-rs provider
    perl # ring
  ];

  # cmake is a build dependency of a crate, not of this package.
  dontUseCmakeConfigure = true;

  buildInputs = [
    glib
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
  ];

  postInstall = ''
    wrapProgram $out/bin/gnome-shell-cast-daemon \
      --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : "${lib.makeSearchPathOutput "lib" "lib/gstreamer-1.0" gstPlugins}" \
      --prefix PATH : ${lib.makeBinPath [ pulseaudio ]}

    install -d $out/share/dbus-1/services
    substitute data/org.gnome.ShellCast.service.in \
      $out/share/dbus-1/services/org.gnome.ShellCast.service \
      --replace-fail '@BINDIR@' "$out/bin"
  '';

  meta = {
    description = "Casts the GNOME desktop to Chromecast devices, controlled over D-Bus by the gnome-shell-cast extension";
    homepage = "https://github.com/omid/gnome-shell-cast";
    license = lib.licenses.mit;
    mainProgram = "gnome-shell-cast-daemon";
    platforms = lib.platforms.linux;
  };
}
