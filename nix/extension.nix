{
  lib,
  stdenvNoCC,
  gnumake,
  gettext,
  glib,
}:

let
  uuid = "gnome-shell-cast@oxygenws.com";
  metadata = lib.importJSON ../extension/${uuid}/metadata.json;
in
stdenvNoCC.mkDerivation {
  pname = "gnome-shell-extension-gnome-shell-cast";
  # The daemon must report "<metadata version>.0.0" or the extension nags to
  # install one; both halves are read from the same source tree to keep that so.
  version = "${toString metadata.version}.0.0";

  src = lib.cleanSource ../.;

  nativeBuildInputs = [
    gnumake
    gettext
    glib
  ];

  dontConfigure = true;

  buildPhase = ''
    runHook preBuild
    make compile-translations
    glib-compile-schemas "extension/${uuid}/schemas"
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    install -d $out/share/gnome-shell/extensions
    cp -r "extension/${uuid}" "$out/share/gnome-shell/extensions/${uuid}"
    rm -rf "$out/share/gnome-shell/extensions/${uuid}/po"
    rm -f "$out/share/gnome-shell/extensions/${uuid}/.gitignore"
    runHook postInstall
  '';

  passthru = {
    extensionUuid = uuid;
    extensionPortalSlug = "gnome-shell-cast";
  };

  meta = {
    description = "Cast your screen or a single window to a Chromecast from the GNOME top panel";
    homepage = "https://github.com/omid/gnome-shell-cast";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };
}
