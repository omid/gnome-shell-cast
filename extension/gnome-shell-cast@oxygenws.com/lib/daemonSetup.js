'use strict';

import GLib from 'gi://GLib';

import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { SetupDialog } from './setupDialog.js';

const RETRY_DELAY_MS = 2000;

// Checks the installed daemon against this extension's version and drives the
// install/update warning and its dialog. Owns the retry timer it creates.
export class DaemonSetup {
    constructor({ extension, daemon, onWarning, onNotify, onDialog }) {
        this._extension = extension;
        this._daemon = daemon;
        this._onWarning = onWarning;
        this._onNotify = onNotify;
        this._onDialog = onDialog;

        this._version = extension.metadata.version;
        this.requiredVersion = `${this._version}.0.0`;
        this._retryId = 0;
    }

    check() {
        // Each user-initiated check gets one grace retry.
        this._retried = false;
        this._check();
    }

    // Only reachable from the warning item, which is shown after _check()
    // has assigned _setup.
    openDialog() {
        this._onDialog(
            new SetupDialog({
                mode: this._setup.mode,
                command: this._installCommand(),
                currentVersion: this._setup.currentVersion,
                requiredVersion: this.requiredVersion,
                url: this._extension.metadata.url,
            }),
        );
    }

    _check() {
        this._daemon.getVersion((version) => {
            if (version === null) {
                if (!this._retried) {
                    this._retry();
                    return;
                }
                this._setup = { mode: 'install', currentVersion: null };
                this._warn(
                    _('Set up the cast daemon'),
                    _(
                        'The cast daemon isn’t installed yet. Open the menu and click ' +
                            '“Set up the cast daemon” to install it.',
                    ),
                );
            } else if (version !== this.requiredVersion) {
                this._setup = { mode: 'update', currentVersion: version };
                this._warn(
                    _('Update the cast daemon (v%old → v%new)')
                        .replace('%old', version)
                        .replace('%new', this.requiredVersion),
                    _(
                        'The cast daemon (v%old) doesn’t match this version of the ' +
                            'extension (needs v%new). Open the menu to update it.',
                    )
                        .replace('%old', version)
                        .replace('%new', this.requiredVersion),
                );
            } else {
                this._onWarning(null);
            }
        });
    }

    // The D-Bus-activated daemon can take a moment to come up right after login;
    // one retry avoids flashing a spurious warning at boot.
    _retry() {
        this._retried = true;
        if (this._retryId) {
            GLib.source_remove(this._retryId);
            this._retryId = 0;
        }
        this._retryId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, RETRY_DELAY_MS, () => {
            this._retryId = 0;
            this._check();
            return GLib.SOURCE_REMOVE;
        });
    }

    _warn(label, notifyMessage) {
        this._onWarning(label);
        // Notify once per distinct problem, so the tray icon isn't silent when
        // the user hasn't opened the menu yet.
        if (this._lastWarning !== notifyMessage) {
            this._lastWarning = notifyMessage;
            this._onNotify(notifyMessage);
        }
    }

    // Pinned to this extension's version so it installs the matching daemon
    // release; the same command therefore updates it after an extension update.
    _installCommand() {
        const url = this._extension.metadata.url;
        const raw = url.replace('github.com', 'raw.githubusercontent.com');
        return `curl -fsSL ${raw}/refs/tags/v${this._version}/scripts/install.sh | sh -s -- v${this._version}`;
    }

    destroy() {
        if (this._retryId) {
            GLib.source_remove(this._retryId);
            this._retryId = 0;
        }
        this._daemon = null;
        this._onWarning = null;
        this._onNotify = null;
        this._onDialog = null;
    }
}
