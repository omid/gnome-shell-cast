'use strict';

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import { Slider } from 'resource:///org/gnome/shell/ui/slider.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastDaemon, SOURCE_AUDIO, SOURCE_CHOOSE, SOURCE_SCREEN } from './daemon.js';
import { CastVolumeControl } from './volumeControl.js';
import { DaemonSetup } from './daemonSetup.js';
import { ErrorDialog } from './errorDialog.js';

const RESOLUTIONS = {
    2160: [3840, 2160],
    1440: [2560, 1440],
    1080: [1920, 1080],
    720: [1280, 720],
};

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

// Mirrors the daemon's rule: VA-API, NVENC and V4L2 elements are hardware.
function isHardwareEncoder(element) {
    return element.startsWith('va') || element.startsWith('nv') || element.startsWith('v4l2');
}

function createMenuItem(label, icon, styleClass = null) {
    const item = new PopupMenu.PopupImageMenuItem(label, icon);
    if (styleClass) item.label.add_style_class_name(styleClass);
    return item;
}

// 'icon-button' is the shell's own; stylesheet.css restyles the rest.
function createRowButton(iconName, label, onClick) {
    const button = new St.Button({
        style_class: 'icon-button flat gsc-row-button',
        can_focus: true,
        child: new St.Icon({ icon_name: iconName, style_class: 'popup-menu-icon' }),
    });
    button.accessible_name = label;
    button.connect('clicked', onClick);
    return button;
}

function toggleStyleClass(element, className, enabled = true) {
    if (enabled) element.add_style_class_name(className);
    else element.remove_style_class_name(className);
}

export function loadIcons(extension) {
    return {
        idle: Gio.icon_new_for_string(`${extension.path}/icons/cast-symbolic.svg`),
        active: Gio.icon_new_for_string(`${extension.path}/icons/cast-connected-symbolic.svg`),
    };
}

// Drives the cast menu contents inside a host-provided PopupMenu, shared by the
// top-bar indicator and the quick-settings toggle.
export class CastMenu {
    constructor({
        extension,
        settings,
        menu,
        icons,
        setIcon,
        onCastChanged,
        onVolume,
        inlineVolume,
        closeMenu,
    }) {
        this._extension = extension;
        this._settings = settings;
        this._menu = menu;
        this._icons = icons;
        this._setIcon = setIcon;
        // The quick-settings host drives its own grid slider through these hooks;
        // the top-bar host sets `inlineVolume` for a slider row in this menu.
        this._onCastChanged = onCastChanged;
        this._onVolume = onVolume;
        this._inlineVolume = inlineVolume;
        // A toggle menu is not a child of the quick-settings menu, so closing
        // it leaves the panel up; the host closes that.
        this._closeMenu = closeMenu;

        this.version = extension.metadata.version;

        this._devices = [];
        this._state = 'idle';
        this._activeDeviceId = '';

        this._daemon = new CastDaemon({
            onDevicesChanged: () => this._refreshDevices(),
            onStateChanged: (state, deviceId) => this._setState(state, deviceId),
            onVolumeChanged: (level) => this._onVolumeChanged(level),
            onDaemonGone: () => this._onDaemonGone(),
            onError: (message) => this._notifyError(message),
            onStartError: (message) => this._showError(message),
        });

        this._daemonSetup = new DaemonSetup({
            extension,
            daemon: this._daemon,
            onWarning: (label) => this._setDaemonWarning(label),
            onNotify: (message) => this._notifyError(message),
            onDialog: (dialog) => this._showDialog(dialog),
        });

        this._buildMenu();

        // Lets the destructive/warning tints switch to their light-popup
        // variants (see stylesheet.css).
        this._stSettings = St.Settings.get();
        this._stSettings.connectObject(
            'notify::color-scheme',
            () => this._updateColorScheme(),
            this,
        );
        this._updateColorScheme();

        this._settings.connectObject(
            'changed::show-details',
            () => this._onShowDetailsChanged(),
            this,
        );

        menu.connectObject(
            'open-state-changed',
            (_menu, open) => {
                if (open) this.refresh();
            },
            this,
        );

        // Reflect an already-running cast right away, without waking an idle daemon.
        this._daemon.getStatus((state, deviceId) => this._setState(state, deviceId), {
            noAutoStart: true,
        });
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

    // Update the quick-settings grid slider (via the hook) and/or this menu's
    // inline slider.
    _onVolumeChanged(level) {
        this._onVolume?.(level);
        this._volumeControl?.setFromDaemon(level);
    }

    activeDeviceName() {
        return this._devices.find((d) => d.id === this._activeDeviceId)?.name ?? _('Cast');
    }

    refresh() {
        this._refreshDevices();
        this._daemon.getStatus((state, deviceId) => this._setState(state, deviceId));
        this._daemonSetup.check();
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
        const light = this._stSettings.color_scheme === St.SystemColorScheme.PREFER_LIGHT;
        toggleStyleClass(this._menu.box, 'gsc-light', light);
    }

    _buildMenu() {
        this._daemonWarningItem = createMenuItem(
            '',
            'dialog-warning-symbolic',
            'gsc-warning-label',
        );
        this._daemonWarningItem.visible = false;
        this._daemonWarningItem.connect('activate', () => this._daemonSetup.openDialog());
        this._menu.addMenuItem(this._daemonWarningItem);

        this._devicesSection = new PopupMenu.PopupMenuSection();
        this._menu.addMenuItem(this._devicesSection);

        this._menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        if (this._inlineVolume) this._buildVolumeItem();

        this._stopItem = createMenuItem(
            _('Stop casting'),
            'media-playback-stop-symbolic',
            'gsc-destructive-label',
        );
        this._stopItem.connect('activate', () => this._daemon.stopCast());
        this._stopItem.visible = false;
        this._menu.addMenuItem(this._stopItem);

        const prefsItem = createMenuItem(_('Preferences'), 'preferences-system-symbolic');
        prefsItem.connect('activate', () => this._extension.openPreferences());
        this._menu.addMenuItem(prefsItem);

        this._rebuildDeviceItems();
    }

    _buildVolumeItem() {
        const item = new PopupMenu.PopupBaseMenuItem({ activate: false });
        const slider = new Slider(0);
        slider.x_expand = true;
        item.add_child(
            new St.Icon({
                icon_name: 'audio-volume-high-symbolic',
                style_class: 'popup-menu-icon',
            }),
        );
        item.add_child(slider);
        item.visible = false;
        this._menu.addMenuItem(item);

        this._volumeItem = item;
        this._volumeControl = new CastVolumeControl(slider, (level) =>
            this._daemon.setVolume(level),
        );
    }

    _setDaemonWarning(label) {
        if (label === null) {
            this._daemonWarningItem.visible = false;
            return;
        }
        this._daemonWarningItem.label.text = label;
        this._daemonWarningItem.visible = true;
    }

    // Tracks the open modal so destroy() can close it: a dialog is parented to
    // the shell, not to us, so it would otherwise outlive disable().
    _showDialog(dialog) {
        this._dialog?.close();
        this._dialog = dialog;
        dialog.connect('closed', () => {
            if (this._dialog === dialog) this._dialog = null;
        });
        dialog.open();
    }

    _refreshDevices() {
        this._daemon.listDevices((devices) => {
            this._devices = devices;
            this._rebuildDeviceItems();
        });
    }

    _markCastingDevice(item, active, deviceName) {
        if (active) {
            toggleStyleClass(item.label, 'gsc-casting-label', true);
            item.label.text = _('%s (casting)').replace('%s', deviceName);
        }
    }

    _rebuildDeviceItems() {
        this._devicesSection.removeAll();

        if (this._devices.length === 0) {
            const empty = new PopupMenu.PopupMenuItem(_('Searching for Chromecast devices…'));
            empty.setSensitive(false);
            this._devicesSection.addMenuItem(empty);
            return;
        }

        const casting = this.casting;

        for (const device of this._devices) {
            const active = casting && device.id === this._activeDeviceId;

            if (!device.hasVideo) {
                const audioItem = createMenuItem(device.name, 'audio-speakers-symbolic');
                this._markCastingDevice(audioItem, active, device.name);
                audioItem.connect('activate', () => this._startCast(device, SOURCE_AUDIO));
                this._devicesSection.addMenuItem(audioItem);
                if (active) this._addDetailLines();
                continue;
            }

            const item = this._createDeviceRow(device, active);
            this._devicesSection.addMenuItem(item);
            if (active) this._addDetailLines();
        }
    }

    // Name on the left, the two cast actions as buttons on the right.
    _createDeviceRow(device, active) {
        const item = new PopupMenu.PopupBaseMenuItem({ activate: false });

        item.add_child(
            new St.Icon({
                gicon: active ? this._icons.active : this._icons.idle,
                style_class: 'popup-menu-icon',
            }),
        );

        // label_actor so the row reads as the device name.
        item.label = new St.Label({
            text: device.name,
            x_expand: true,
            y_expand: true,
            y_align: Clutter.ActorAlign.CENTER,
        });
        item.add_child(item.label);
        item.label_actor = item.label;
        this._markCastingDevice(item, active, device.name);

        item.add_child(
            createRowButton('video-display-symbolic', _('Cast screen'), () =>
                this._startCast(device, SOURCE_SCREEN),
            ),
        );
        item.add_child(
            createRowButton('focus-windows-symbolic', _('Choose what to cast'), () =>
                this._startCast(device, SOURCE_CHOOSE),
            ),
        );

        return item;
    }

    // Populated from GetDetails when the "show details" setting is on.
    _addDetailLines() {
        if (!this._details || !this._settings.get_boolean('show-details')) return;
        const { transport, codec, encoder, format, receiverCodecs } = this._details;
        if (!transport) return;
        const TRANSPORT_LABELS = { mirror: _('Cast streaming'), audio: _('Audio stream') };
        const transportLabel = TRANSPORT_LABELS[transport] ?? _('HLS');
        this._addDetailLine(codec ? `${transportLabel} · ${formatCodec(codec)}` : transportLabel);
        // Which encoder and pixel format were picked is worth showing precisely
        // because both settings default to automatic. The format arrives a
        // moment later than the encoder, once the pipeline has negotiated.
        if (encoder) {
            const line = (
                isHardwareEncoder(encoder)
                    ? _('Encoder: %s (hardware)')
                    : _('Encoder: %s (software)')
            ).replace('%s', encoder);
            this._addDetailLine(format ? `${line} · ${format}` : line);
        }
        if (receiverCodecs.length > 0)
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
        // The row is not activatable, so nothing closes the menu for us.
        this._menu.itemActivated();
        this._closeMenu?.();
    }

    _castOptions() {
        const options = {
            fps: new GLib.Variant('i', this._settings.get_int('fps')),
            'bitrate-kbps': new GLib.Variant('i', this._settings.get_int('bitrate-kbps')),
            'audio-bitrate-kbps': new GLib.Variant(
                'i',
                this._settings.get_int('audio-bitrate-kbps'),
            ),
            'video-encoder': new GLib.Variant('s', this._settings.get_string('video-encoder')),
            'video-codec': new GLib.Variant('s', this._settings.get_string('video-codec')),
            'video-format': new GLib.Variant('s', this._settings.get_string('video-format')),
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

        this._reflectState();

        // Codecs are known only once a cast is actually running.
        if (state === 'casting' && this._settings.get_boolean('show-details')) {
            this._daemon.getDetails((details) => {
                this._details = details;
                this._rebuildDeviceItems();
            });
        } else {
            this._details = null;
        }

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

    _reflectState() {
        this._setIcon(this.casting);
        this._stopItem.visible = this.casting;
        this._onCastChanged?.(this.casting, this.activeDeviceName());
        if (this._volumeItem) {
            this._volumeItem.visible = this.casting;
            if (this.casting) {
                this._daemon.getVolume((level) => {
                    if (level !== null) this._volumeControl.setFromDaemon(level);
                });
            }
        }
    }

    // Daemon gone without a final state update: reset to "not casting" without
    // calling back into D-Bus, which would just reactivate it.
    _onDaemonGone() {
        if (this._state === 'idle') return;
        this._state = 'idle';
        this._activeDeviceId = '';
        this._details = null;
        this._reflectState();
        this._rebuildDeviceItems();
    }

    _showError(message) {
        // Don't re-pop the window for the same error.
        if (this._lastErrorShown === message) return;
        this._lastErrorShown = message;
        const dialog = new ErrorDialog({
            message,
            version: this.version,
            url: this._extension.metadata.url,
        });
        dialog.connect('closed', () => {
            if (this._lastErrorShown === message) this._lastErrorShown = null;
        });
        this._showDialog(dialog);
    }

    _notifyError(message) {
        Main.notify(_('GNOME Shell Cast'), message);
    }

    destroy() {
        this._daemonSetup.destroy();
        this._daemonSetup = null;
        this._stSettings.disconnectObject(this);
        this._settings.disconnectObject(this);
        this._menu.disconnectObject(this);
        // close() pops the modal grab; ModalDialog destroys itself on close.
        this._dialog?.close();
        this._dialog = null;
        if (this._volumeControl) {
            this._volumeControl.destroy();
            this._volumeControl = null;
        }
        this._daemon.destroy();
        this._daemon = null;
    }
}
