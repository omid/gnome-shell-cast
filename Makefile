#!/usr/bin/make -f

_UUID = gnome-shell-cast@oxygenws.com
_EXT_DIR = extension/$(_UUID)
_DAEMON_BIN = daemon/target/release/gnome-shell-cast-daemon
ifeq ($(strip $(DESTDIR)),)
	_EXT_INSTALL_BASE = $(HOME)/.local/share/gnome-shell/extensions
	_BIN_INSTALL_DIR = $(HOME)/.local/bin
	_DBUS_SERVICE_DIR = $(HOME)/.local/share/dbus-1/services
else
	_EXT_INSTALL_BASE = $(DESTDIR)/usr/share/gnome-shell/extensions
	_BIN_INSTALL_DIR = $(DESTDIR)/usr/bin
	_DBUS_SERVICE_DIR = $(DESTDIR)/usr/share/dbus-1/services
endif

.PHONY: all daemon install-local install-extension install-daemon uninstall-local clean eslint ego-zip zip tailLog

all: daemon

daemon:
	cd daemon && cargo build --release

install-local: install-extension install-daemon

install-extension:
	glib-compile-schemas $(_EXT_DIR)/schemas/
	rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	mkdir -p $(_EXT_INSTALL_BASE)/$(_UUID)
	cp -r $(_EXT_DIR)/* $(_EXT_INSTALL_BASE)/$(_UUID)/

# Standalone daemon install, for users who got the extension itself from
# extensions.gnome.org (the daemon cannot be distributed there).
install-daemon: daemon
	mkdir -p $(_BIN_INSTALL_DIR)
	install -m755 $(_DAEMON_BIN) $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	mkdir -p $(_DBUS_SERVICE_DIR)
	sed 's|@BINDIR@|$(_BIN_INSTALL_DIR)|' data/org.gnome.ShellCast.service.in \
		> $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

uninstall-local:
	rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	rm -f $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	rm -f $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

clean:
	rm -rf build/ daemon/target/ $(_EXT_DIR)/schemas/gschemas.compiled

eslint:
	npx eslint $(_EXT_DIR)

# Builds the reviewable extension package for extensions.gnome.org.
# EGO only accepts pure-JS extensions — no compiled binaries — so the Rust
# daemon is deliberately NOT part of this zip; users install it with
# `make install-daemon`. Upload the zip at https://extensions.gnome.org/upload/
ego-zip: export _VERSION=$(shell jq '.version' $(_EXT_DIR)/metadata.json)
ego-zip: eslint
	rm -f $(_EXT_DIR)/schemas/gschemas.compiled
	gnome-extensions pack --force --out-dir=. \
		--extra-source=indicator.js --extra-source=daemon.js --extra-source=icons \
		--schema=$(_EXT_DIR)/schemas/org.gnome.shell.extensions.gnome-shell-cast.gschema.xml \
		$(_EXT_DIR)
	mv "$(_UUID).shell-extension.zip" "$(_UUID).v$(_VERSION).zip"
	@echo "Upload $(_UUID).v$(_VERSION).zip at https://extensions.gnome.org/upload/"

zip: ego-zip

tailLog:
	journalctl -f -g gnome-shell-cast
