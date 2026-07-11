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

.PHONY: all daemon install-local uninstall-local clean eslint zip tailLog

all: daemon

daemon:
	cd daemon && cargo build --release

install-local: daemon
	# Extension
	glib-compile-schemas $(_EXT_DIR)/schemas/
	rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	mkdir -p $(_EXT_INSTALL_BASE)/$(_UUID)
	cp -r $(_EXT_DIR)/* $(_EXT_INSTALL_BASE)/$(_UUID)/
	# Daemon
	mkdir -p $(_BIN_INSTALL_DIR)
	install -m755 $(_DAEMON_BIN) $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	# D-Bus activation
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

zip:
	glib-compile-schemas $(_EXT_DIR)/schemas/
	gnome-extensions pack --force --out-dir=. \
		--extra-source=indicator.js --extra-source=daemon.js --extra-source=icons \
		$(_EXT_DIR)

tailLog:
	journalctl -f -g gnome-shell-cast
