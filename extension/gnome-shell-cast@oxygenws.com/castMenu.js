'use strict';

import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastDaemon, SOURCE_AUDIO, SOURCE_SCREEN, SOURCE_WINDOW } from './daemon.js';
import { SetupDialog } from './setupDialog.js';
import { ErrorDialog } from './errorDialog.js';

const RESOLUTIONS = {
    1080: [1920, 1080],
    720: [1280, 720],
};

// Display names for the lowercase codec ids the daemon reports (Cast
// protocol / GStreamer element names).
const CODEC_LABELS = {
    h264: 'H.264',
    vp8: 'VP8',
    vp9: 'VP9',
    av1: 'AV1',
    aac: 'AAC',
    mp3: 'MP3',
    opus: 'Opus',
};

function formatCodec(codec) {
    return CODEC_LABELS[codec] ?? codec;
}

export function loadIcons(extension) {
    return {
        idle: Gio.icon_new_for_string(`${extension.path}/icons/cast-symbolic.svg`),
        active: Gio.icon_new_for_string(`${extension.path}/icons/cast-connected-symbolic.svg`),
    };
}

// Builds and drives the cast menu contents (device list, casting state,
// daemon setup warnings) inside a host-provided PopupMenu-compatible menu.
// Shared between the top-bar indicator and the quick-settings toggle so the
// two host widgets don't duplicate this logic.
export class CastMenu {
    constructor({ extension, menu, icons, setIcon, onCastChanged, onVolume }) {
        this._extension = extension;
        this._menu = menu;
        this._icons = icons;
        this._setIcon = setIcon;
        // Optional hooks used by the quick-settings host to drive the cast
        // volume slider; absent for the top-bar indicator.
        this._onCastChanged = onCastChanged;
        this._onVolume = onVolume;

        this.version = extension.metadata.version;
        this.daemonVersion = `${this.version}.0.0`;

        this._settings = extension.getSettings();
        this._devices = [];
        this._state = 'idle';
        this._activeDeviceId = '';

        this._daemon = new CastDaemon({
            onDevicesChanged: () => this._refreshDevices(),
            onStateChanged: (state, deviceId) => this._setState(state, deviceId),
            onVolumeChanged: (level) => this._onVolume?.(level),
            onDaemonGone: () => this._onDaemonGone(),
            onError: (message) => this._notifyError(message),
            onStartError: (message) => this._showError(message),
        });

        this._buildMenu();

        // Track the shell's colour scheme so the destructive/warning tints
        // can switch to their light-popup variants (see stylesheet.css).
        this._stSettings = St.Settings.get();
        this._colorSchemeId = this._stSettings.connect('notify::color-scheme', () =>
            this._updateColorScheme(),
        );
        this._updateColorScheme();

        // Update the detail lines live when the user toggles the setting.
        this._showDetailsId = this._settings.connect('changed::show-details', () =>
            this._onShowDetailsChanged(),
        );
    }

    get casting() {
        return this._state === 'casting' || this._state === 'connecting';
    }

    stopCast() {
        this._daemon.stopCast();
    }

    setVolume(level) {
        this._daemon.setVolume(level);
    }

    getVolume(callback) {
        this._daemon.getVolume(callback);
    }

    // Name of the device the cast is currently going to, for the volume slider.
    activeDeviceName() {
        return this._devices.find((d) => d.id === this._activeDeviceId)?.name ?? 'Cast';
    }

    refresh() {
        // Each user-initiated refresh gets one grace retry (see below).
        this._daemonCheckRetried = false;
        this._refreshDevices();
        this._daemon.getStatus((state, deviceId) => this._setState(state, deviceId));
        this._checkDaemonVersion();
    }

    _onShowDetailsChanged() {
        if (this._settings.get_boolean('show-details') && this._state === 'casting') {
            this._daemon.getDetails((details) => {
                this._details = details;
                this._rebuildDeviceItems();
            });
        } else {
            this._details = null;
        }
        this._rebuildDeviceItems();
    }

    _updateColorScheme() {
        const light = this._stSettings.color_scheme === St.SystemColorScheme?.PREFER_LIGHT;
        if (light) this._menu.box.add_style_class_name('gsc-light');
        else this._menu.box.remove_style_class_name('gsc-light');
    }

    _buildMenu() {
        // Shown only when the daemon is missing or a different version.
        this._daemonWarningItem = new PopupMenu.PopupImageMenuItem('', 'dialog-warning-symbolic');
        this._daemonWarningItem.label.add_style_class_name('gsc-warning-label');
        this._daemonWarningItem.visible = false;
        this._daemonWarningItem.connect('activate', () => this._openSetupDialog());
        this._menu.addMenuItem(this._daemonWarningItem);

        this._devicesSection = new PopupMenu.PopupMenuSection();
        this._menu.addMenuItem(this._devicesSection);

        this._menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._stopItem = new PopupMenu.PopupImageMenuItem(
            _('Stop casting'),
            'media-playback-stop-symbolic',
        );
        this._stopItem.label.add_style_class_name('gsc-destructive-label');
        this._stopItem.connect('activate', () => this._daemon.stopCast());
        this._stopItem.visible = false;
        this._menu.addMenuItem(this._stopItem);

        const prefsItem = new PopupMenu.PopupImageMenuItem(
            _('Preferences'),
            'preferences-system-symbolic',
        );
        prefsItem.connect('activate', () => this._extension.openPreferences());
        this._menu.addMenuItem(prefsItem);

        this._rebuildDeviceItems();
    }

    _checkDaemonVersion() {
        this._daemon.getVersion((version) => {
            if (version === null) {
                // The D-Bus-activated daemon can take a moment to come up
                // right after login; give it one retry before declaring it
                // missing so we don't flash a spurious warning at boot.
                if (!this._daemonCheckRetried) {
                    this._daemonCheckRetried = true;
                    if (!this._versionRetryId) {
                        this._versionRetryId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 2000, () => {
                            this._versionRetryId = 0;
                            this._checkDaemonVersion();
                            return GLib.SOURCE_REMOVE;
                        });
                    }
                    return;
                }
                this._daemonSetup = { mode: 'install', currentVersion: null };
                this._showDaemonWarning(
                    _('Set up the cast daemon'),
                    _(
                        'The cast daemon isn’t installed yet. Open the menu and click ' +
                            '“Set up the cast daemon” to install it.',
                    ),
                );
            } else if (version !== this.daemonVersion) {
                this._daemonSetup = { mode: 'update', currentVersion: version };
                this._showDaemonWarning(
                    _('Update the cast daemon (v%old → v%new)')
                        .replace('%old', version)
                        .replace('%new', this.daemonVersion),
                    _(
                        'The cast daemon (v%old) doesn’t match this version of the ' +
                            'extension (needs v%new). Open the menu to update it.',
                    )
                        .replace('%old', version)
                        .replace('%new', this.daemonVersion),
                );
            } else {
                this._daemonWarningItem.visible = false;
            }
        });
    }

    _showDaemonWarning(label, notifyMessage) {
        this._daemonWarningItem.label.text = label;
        this._daemonWarningItem.visible = true;
        // Notify once per distinct problem so the tray icon isn't silent
        // when the user hasn't opened the menu yet.
        if (this._lastDaemonWarning !== notifyMessage) {
            this._lastDaemonWarning = notifyMessage;
            this._notifyError(notifyMessage);
        }
    }

    _daemonRepoUrl() {
        return this._extension.metadata?.url ?? 'https://github.com/omid/gnome-shell-cast';
    }

    // The one-liner the setup/update dialog shows. Pinned to this
    // extension's version so it installs the matching daemon release - the
    // same command therefore updates the daemon after an extension update.
    _installCommand() {
        const version = this._extension.metadata.version;
        const raw = this._daemonRepoUrl().replace('github.com', 'raw.githubusercontent.com');
        return `curl -fsSL ${raw}/refs/tags/v${version}/scripts/install.sh | sh -s -- v${version}`;
    }

    _openSetupDialog() {
        const setup = this._daemonSetup ?? { mode: 'install', currentVersion: null };
        const dialog = new SetupDialog({
            mode: setup.mode,
            command: this._installCommand(),
            currentVersion: setup.currentVersion,
            requiredVersion: this.daemonVersion,
            url: this._daemonRepoUrl(),
        });
        dialog.open();
    }

    _refreshDevices() {
        this._daemon.listDevices((devices) => {
            this._devices = devices;
            this._rebuildDeviceItems();
        });
    }

    _rebuildDeviceItems() {
        this._devicesSection.removeAll();

        if (this._devices.length === 0) {
            const empty = new PopupMenu.PopupMenuItem('Searching for Chromecast devices…');
            empty.setSensitive(false);
            this._devicesSection.addMenuItem(empty);
            return;
        }

        const casting = this.casting;

        for (const device of this._devices) {
            const active = casting && device.id === this._activeDeviceId;

            // Audio-only devices (speakers, cast groups) get a single
            // item that shares system audio; a screen/window submenu
            // would be meaningless for them.
            if (!device.hasVideo) {
                const audioItem = new PopupMenu.PopupImageMenuItem(
                    device.name,
                    'audio-speakers-symbolic',
                );
                if (active) {
                    audioItem.label.add_style_class_name('gsc-casting-label');
                    audioItem.label.text = _('%s (casting)').replace('%s', device.name);
                }
                audioItem.connect('activate', () => this._startCast(device, SOURCE_AUDIO));
                this._devicesSection.addMenuItem(audioItem);
                if (active) this._addDetailLines();
                continue;
            }

            const item = new PopupMenu.PopupSubMenuMenuItem(device.name, true);
            item.icon.gicon = active ? this._icons.active : this._icons.idle;
            if (active) {
                // Mark the device we are currently casting to with the
                // system accent colour.
                item.label.add_style_class_name('gsc-casting-label');
                item.label.text = _('%s (casting)').replace('%s', device.name);
            }

            const screenItem = new PopupMenu.PopupImageMenuItem(
                _('Cast screen'),
                'video-display-symbolic',
            );
            screenItem.connect('activate', () => this._startCast(device, SOURCE_SCREEN));
            item.menu.addMenuItem(screenItem);

            const windowItem = new PopupMenu.PopupImageMenuItem(
                _('Cast window'),
                'window-new-symbolic',
            );
            windowItem.connect('activate', () => this._startCast(device, SOURCE_WINDOW));
            item.menu.addMenuItem(windowItem);

            this._devicesSection.addMenuItem(item);
            if (active) this._addDetailLines();
        }
    }

    // Dim, non-interactive lines under the active device showing the
    // transport and negotiated codecs. Populated from GetDetails when the
    // "show details" setting is on; a no-op otherwise.
    _addDetailLines() {
        if (!this._details || !this._settings.get_boolean('show-details')) return;
        const { transport, codec, receiverCodecs } = this._details;
        if (!transport) return;
        const TRANSPORT_LABELS = { mirror: _('Cast streaming'), audio: _('Audio stream') };
        const transportLabel = TRANSPORT_LABELS[transport] ?? _('HLS');
        this._addDetailLine(codec ? `${transportLabel} · ${formatCodec(codec)}` : transportLabel);
        if (receiverCodecs && receiverCodecs.length > 0)
            this._addDetailLine(
                _('Receiver supports: %s').replace(
                    '%s',
                    receiverCodecs.map(formatCodec).join(', '),
                ),
            );
    }

    _addDetailLine(text) {
        const item = new PopupMenu.PopupMenuItem(text);
        item.setSensitive(false);
        item.label.add_style_class_name('gsc-detail-line');
        this._devicesSection.addMenuItem(item);
    }

    _startCast(device, source) {
        this._daemon.startCast(device.id, source, this._castOptions());
    }

    _castOptions() {
        const options = {
            fps: new GLib.Variant('i', this._settings.get_int('fps')),
            'bitrate-kbps': new GLib.Variant('i', this._settings.get_int('bitrate-kbps')),
        };

        const size = RESOLUTIONS[this._settings.get_string('resolution')];
        if (size) {
            options.width = new GLib.Variant('i', size[0]);
            options.height = new GLib.Variant('i', size[1]);
        }

        return options;
    }

    _setState(state, deviceId) {
        const prev = this._state;
        this._state = state;
        this._activeDeviceId = deviceId;

        this._setIcon(this.casting);
        this._stopItem.visible = this.casting;

        // Let the quick-settings host show/hide the cast volume slider.
        this._onCastChanged?.(this.casting, this.activeDeviceName());

        // Codecs are known only once a cast is actually running; fetch them
        // then, and rebuild once they arrive. Otherwise clear them.
        if (state === 'casting' && this._settings.get_boolean('show-details')) {
            this._daemon.getDetails((details) => {
                this._details = details;
                this._rebuildDeviceItems();
            });
        } else {
            this._details = null;
        }

        // Reflect the active device highlight in the device list.
        this._rebuildDeviceItems();

        // A genuine failure pops the error window with the real reason; a
        // device that just disconnected gets a notification instead.
        if (state === 'error' && prev !== 'error') {
            this._daemon.getLastEvent(({ message }) =>
                this._showError(message || _('The cast failed.')),
            );
        } else if (state === 'idle' && (prev === 'casting' || prev === 'connecting')) {
            this._daemon.getLastEvent(({ kind, message }) => {
                if (kind === 'ended') {
                    this._notifyError(
                        message
                            ? _('The device ended the session (%s).').replace('%s', message)
                            : _('The device ended the session.'),
                    );
                }
            });
        }
    }

    // Daemon gone without a final state update: reset to "not casting" without
    // calling back into D-Bus (which would just reactivate it).
    _onDaemonGone() {
        if (this._state === 'idle') return;
        this._state = 'idle';
        this._activeDeviceId = '';
        this._details = null;
        this._setIcon(false);
        this._stopItem.visible = false;
        this._onCastChanged?.(false, this.activeDeviceName());
        this._rebuildDeviceItems();
    }

    _showError(message) {
        // Don't re-pop the window for the same error.
        if (this._lastErrorShown === message) return;
        this._lastErrorShown = message;
        const dialog = new ErrorDialog({
            message,
            version: this._extension.metadata.version,
            url: this._daemonRepoUrl(),
        });
        dialog.connect('closed', () => {
            this._lastErrorShown = null;
        });
        dialog.open();
    }

    _notifyError(message) {
        Main.notify('GNOME Shell Cast', message);
    }

    destroy() {
        if (this._versionRetryId) {
            GLib.source_remove(this._versionRetryId);
            this._versionRetryId = 0;
        }
        if (this._colorSchemeId) {
            this._stSettings.disconnect(this._colorSchemeId);
            this._colorSchemeId = null;
        }
        if (this._showDetailsId) {
            this._settings.disconnect(this._showDetailsId);
            this._showDetailsId = null;
        }
        this._daemon.destroy();
        this._daemon = null;
    }
}
