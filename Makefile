#!/usr/bin/make -f

_UUID = gnome-shell-cast@oxygenws.com
_EXT_DIR = extension/$(_UUID)
_DAEMON_BIN = daemon/target/release/gnome-shell-cast-daemon
PREFIX ?= /usr
# Where the files end up on the target machine. DESTDIR only stages the install
# tree, so it must never reach anything baked into a file - see _BINDIR below.
ifeq ($(strip $(DESTDIR)),)
	_BINDIR = $(HOME)/.local/bin
	_DATADIR = $(HOME)/.local/share
else
	_BINDIR = $(PREFIX)/bin
	_DATADIR = $(PREFIX)/share
endif
_EXT_INSTALL_BASE = $(DESTDIR)$(_DATADIR)/gnome-shell/extensions
_BIN_INSTALL_DIR = $(DESTDIR)$(_BINDIR)
_DBUS_SERVICE_DIR = $(DESTDIR)$(_DATADIR)/dbus-1/services

.PHONY: all daemon install install-extension install-daemon uninstall-local set-version release clean eslint eslint-fix ego-zip zip shexli tailLog check check_nightly check_strictly

all: daemon

daemon:
	@cd daemon && cargo build --release

install: install-extension install-daemon

install-extension: compile-translations
	@glib-compile-schemas $(_EXT_DIR)/schemas/
	@rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	@mkdir -p $(_EXT_INSTALL_BASE)/$(_UUID)
	@cp -r $(_EXT_DIR)/* $(_EXT_INSTALL_BASE)/$(_UUID)/
	@rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)/po

# Standalone daemon install, for users who got the extension itself from
# extensions.gnome.org (the daemon cannot be distributed there).
install-daemon: daemon
	@mkdir -p $(_BIN_INSTALL_DIR)
	@install -m755 $(_DAEMON_BIN) $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	@mkdir -p $(_DBUS_SERVICE_DIR)
	@sed 's|@BINDIR@|$(_BINDIR)|' data/org.gnome.ShellCast.service.in \
		> $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

uninstall-local:
	@rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	@rm -f $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	@rm -f $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

# Nested GNOME Shell for trying a change without logging out - the shell caches
# ES modules per process, so edited JS only takes effect in a fresh one. Its own
# session bus means the daemon is activated freshly too; installs first, or the
# nested shell would run the previously installed copy.
.PHONY: run-nested
run-nested: install ## Run the extension in a nested GNOME Shell (no logout needed).
	@dbus-run-session gnome-shell --devkit --wayland

# Set the single project version everywhere (usage: make set-version V=2).
set-version:
	@sh scripts/set-version.sh $(V)

# Interactive: bump the version, run checks, build the zip, commit, tag and
# push. The tag push triggers the release workflow that publishes the daemon
# binaries. Override the version with V=<n>.
release:
	@sh scripts/release.sh

clean:
	@rm -rf build/ daemon/target/ $(_EXT_DIR)/schemas/gschemas.compiled $(_EXT_DIR)/locale

eslint:
	@yarn install
	@npx eslint $(_EXT_DIR) eslint.config.mjs

eslint-fix:
	@yarn install
	@npx eslint --fix $(_EXT_DIR) eslint.config.mjs

# Builds the reviewable extension package for extensions.gnome.org.
# EGO only accepts pure-JS extensions - no compiled binaries - so the Rust
# daemon is deliberately NOT part of this zip; users install it with
# `make install-daemon`. Upload the zip at https://extensions.gnome.org/upload/
ego-zip: export _VERSION=$(shell jq '.version' $(_EXT_DIR)/metadata.json)
ego-zip: eslint
	@rm -f $(_EXT_DIR)/schemas/gschemas.compiled
	@gnome-extensions pack --force --out-dir=. \
		--extra-source=lib --extra-source=icons \
		--schema=schemas/org.gnome.shell.extensions.gnome-shell-cast.gschema.xml \
		--podir=po --gettext-domain=$(_UUID) \
		$(_EXT_DIR)
	@mv "$(_UUID).shell-extension.zip" "$(_UUID).v$(_VERSION).zip"
	@echo "Upload $(_UUID).v$(_VERSION).zip at https://extensions.gnome.org/upload/"

zip: ego-zip

tailLog:
	@journalctl -f -g gnome-shell-cast

shexli: export _VERSION=$(shell jq '.version' $(_EXT_DIR)/metadata.json)
shexli: zip
	@uv venv --allow-existing
	@uv pip install shexli
	@.venv/bin/shexli "$(_UUID).v$(_VERSION).zip"


.PHONY: check
check: ## Fast type-check without producing artifacts for all shared crates.
	@(cd daemon && cargo check --all-targets) || exit 1;

# ---------------------------------------------------------------- test

.PHONY: test
test: ## Run the full test suite for all shared crates.
	@(cd daemon && cargo test) || exit 1;

.PHONY: test-doc
test-doc: ## Run doctests only for all shared crates.
	@(cd daemon && cargo test --doc) || exit 1;

# ---------------------------------------------------------------- lint / fmt

.PHONY: fmt
fmt: ## Format the code in-place for all shared crates.
	@(cd daemon && cargo fmt --all) || exit 1;

.PHONY: fmt-check
fmt-check: ## Verify formatting without modifying files (CI mode).
	@(cd daemon && cargo fmt --all -- --check) || exit 1;

.PHONY: clippy
clippy: ## Run clippy, denying warnings.
	@(cd daemon && cargo clippy --all-targets -- -D warnings) || exit 1;

.PHONY: clippy-fix
clippy-fix: ## Run clippy, denying warnings.
	@(cd daemon && cargo clippy --all-targets --fix --allow-dirty -- -D warnings) || exit 1;

.PHONY: fmt-js
fmt-js: ## Format the extension JS in-place with Prettier.
	@npx prettier --write extension/ eslint.config.mjs

.PHONY: fmt-js-check
fmt-js-check: ## Verify extension JS formatting without modifying files (CI mode).
	@npx prettier --check extension/ eslint.config.mjs

# ---------------------------------------------------------------- docs

.PHONY: doc
doc: ## Build rustdoc (no dependencies).
	@(cd daemon && cargo doc --no-deps) || exit 1;

# ---------------------------------------------------------------- meta

.PHONY: update-dry-run
update-dry-run: ## Check for available updates without modifying Cargo.lock.
	@(cd daemon && cargo update --dry-run) || exit 1;

.PHONY: check-unused-deps
check-unused-deps: ## Check for unused dependencies (requires cargo-machete).
	@(cd daemon && cargo machete) || exit 1;

.PHONY: check-outdated
check-outdated: ## Check for outdated dependencies (requires cargo-outdated).
	@(cd daemon && cargo outdated -wR) || exit 1;

.PHONY: sort-toml
sort-toml: ## Sort Cargo.toml fields in-place (requires cargo-sort).
	@(cd daemon && cargo sort -wg) || exit 1;

.PHONY: sort-check
sort-check: ## Verify Cargo.toml is sorted without modifying it (requires cargo-sort).
	@(cd daemon && cargo sort -cg) || exit 1;

# ---------------------------------------------------------------- translations

.PHONY: extract-translations
extract-translations: ## Extract translatable strings from source files.
	@mkdir -p $(_EXT_DIR)/po
	@xgettext \
		--copyright-holder="GNOME Shell Cast contributors" \
		--package-name="GNOME Shell Cast" \
		--package-version="$$(jq -r '.version' $(_EXT_DIR)/metadata.json)" \
		--msgid-bugs-address="https://github.com/omid/gnome-shell-cast/issues" \
		--default-domain="gnome-shell-cast@oxygenws.com" \
		--output="$(_EXT_DIR)/po/gnome-shell-cast@oxygenws.com.pot" \
		--from-code=UTF-8 \
		--add-comments=translators: \
		--keyword=_ \
		--keyword=_:1,2 \
		$$(find $(_EXT_DIR) -name "*.js" -type f | sort) \
		$(_EXT_DIR)/schemas/*.gschema.xml
	@echo "✓ Extracted strings to $(_EXT_DIR)/po/gnome-shell-cast@oxygenws.com.pot"

.PHONY: update-translations
update-translations: ## Update .po files from .pot template (preserves existing translations).
	@for po_file in $(_EXT_DIR)/po/*.po; do \
		if [ -f "$$po_file" ]; then \
			msgmerge --update --backup=none "$$po_file" "$(_EXT_DIR)/po/$(_UUID).pot"; \
		fi \
	done
	@echo "✓ Updated .po files from .pot template"

.PHONY: compile-translations
compile-translations: ## Compile .po files into the locale/ tree shipped with the extension.
	@rm -rf $(_EXT_DIR)/locale
	@for po_file in $(_EXT_DIR)/po/*.po; do \
		[ -f "$$po_file" ] || continue; \
		lang=$$(basename "$$po_file" .po); \
		mkdir -p "$(_EXT_DIR)/locale/$$lang/LC_MESSAGES"; \
		msgfmt "$$po_file" -o "$(_EXT_DIR)/locale/$$lang/LC_MESSAGES/$(_UUID).mo"; \
	done
	@echo "✓ Compiled .po files into $(_EXT_DIR)/locale/"

.PHONY: translations
translations: extract-translations update-translations compile-translations ## Full translation workflow: extract → update → compile.

# ---------------------------------------------------------------- everything

.PHONY: check-all
check-all: fmt-check clippy test eslint fmt-js-check update-dry-run check-unused-deps check-outdated sort-check shexli ## Check ALL (no changes): daemon fmt+clippy+test, extension eslint+prettier, dependency hygiene, Cargo.toml sort, and shexli EGO validation. This is what CI should run.

.PHONY: fix-all
fix-all: fmt clippy-fix fmt-js eslint-fix sort-toml ## Fix ALL in-place: daemon fmt+clippy-fix, extension prettier+eslint --fix, and Cargo.toml sort.
