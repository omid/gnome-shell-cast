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

.PHONY: all daemon install-local install-extension install-daemon uninstall-local set-version release clean eslint ego-zip zip shexli tailLog check check_nightly check_strictly

all: daemon

daemon:
	@cd daemon && cargo build --release

install-local: install-extension install-daemon

install-extension:
	@glib-compile-schemas $(_EXT_DIR)/schemas/
	@rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	@mkdir -p $(_EXT_INSTALL_BASE)/$(_UUID)
	@cp -r $(_EXT_DIR)/* $(_EXT_INSTALL_BASE)/$(_UUID)/

# Standalone daemon install, for users who got the extension itself from
# extensions.gnome.org (the daemon cannot be distributed there).
install-daemon: daemon
	@mkdir -p $(_BIN_INSTALL_DIR)
	@install -m755 $(_DAEMON_BIN) $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	@mkdir -p $(_DBUS_SERVICE_DIR)
	@sed 's|@BINDIR@|$(_BIN_INSTALL_DIR)|' data/org.gnome.ShellCast.service.in \
		> $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

uninstall-local:
	@rm -rf $(_EXT_INSTALL_BASE)/$(_UUID)
	@rm -f $(_BIN_INSTALL_DIR)/gnome-shell-cast-daemon
	@rm -f $(_DBUS_SERVICE_DIR)/org.gnome.ShellCast.service

# Set the single project version everywhere (usage: make set-version V=2).
set-version:
	@sh scripts/set-version.sh $(V)

# Interactive: bump the version, run checks, build the zip, commit, tag and
# push. The tag push triggers the release workflow that publishes the daemon
# binaries. Override the version with V=<n>.
release:
	@sh scripts/release.sh

clean:
	@rm -rf build/ daemon/target/ $(_EXT_DIR)/schemas/gschemas.compiled

eslint:
	@yarn install
	@npx eslint $(_EXT_DIR) eslint.config.mjs

# Builds the reviewable extension package for extensions.gnome.org.
# EGO only accepts pure-JS extensions — no compiled binaries — so the Rust
# daemon is deliberately NOT part of this zip; users install it with
# `make install-daemon`. Upload the zip at https://extensions.gnome.org/upload/
ego-zip: export _VERSION=$(shell jq '.version' $(_EXT_DIR)/metadata.json)
ego-zip: eslint
	@rm -f $(_EXT_DIR)/schemas/gschemas.compiled
	@gnome-extensions pack --force --out-dir=. \
		--extra-source=indicator.js --extra-source=daemon.js \
		--extra-source=setupDialog.js --extra-source=errorDialog.js \
		--extra-source=icons \
		--schema=schemas/org.gnome.shell.extensions.gnome-shell-cast.gschema.xml \
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
	@uv run shexli "$(_UUID).v$(_VERSION).zip"


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

.PHONY: lint
lint: fmt-check clippy ## fmt-check + clippy.

.PHONY: fmt-js
fmt-js: ## Format the extension JS in-place with Prettier.
	@npx prettier --write extension/ eslint.config.mjs

.PHONY: fmt-js-check
fmt-js-check: ## Verify extension JS formatting without modifying files (CI mode).
	@npx prettier --check extension/ eslint.config.mjs

.PHONY: lint-js
lint-js: eslint fmt-js-check ## eslint + prettier check for the extension.

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
sort-toml: ## Sort Cargo.toml fields (requires cargo-sort).
	@(cd daemon && cargo sort -wg) || exit 1;

.PHONY: check-full
check-full: fmt clippy update-dry-run check-unused-deps check-outdated sort-toml ## Run comprehensive checks on the workspace.

.PHONY: check-fix
check-fix: fmt clippy-fix update-dry-run check-unused-deps check-outdated sort-toml ## Run comprehensive checks on the workspace.

.PHONY: ci
ci: fmt-check clippy test lint-js ## What CI should run: Rust fmt-check + clippy + test, JS eslint + prettier.
