'use strict';

import GObject from 'gi://GObject';
import GLib from 'gi://GLib';
import Gio from 'gi://Gio';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

import { CastDaemon, SOURCE_SCREEN, SOURCE_WINDOW } from './daemon.js';

const RESOLUTIONS = {
    '1080': [1920, 1080],
    '720': [1280, 720],
};

export const CastIndicator = GObject.registerClass(
    class CastIndicator extends PanelMenu.Button {
        _init(extension) {
            super._init(0.5, 'Cast to Chromecast');

            this._extension = extension;
            this._settings = extension.getSettings();
            this._devices = [];
            this._state = 'idle';
            this._activeDeviceId = '';

            this._iconIdle = Gio.icon_new_for_string(
                `${extension.path}/icons/cast-symbolic.svg`);
            this._iconActive = Gio.icon_new_for_string(
                `${extension.path}/icons/cast-connected-symbolic.svg`);
            this._icon = new St.Icon({
                gicon: this._iconIdle,
                style_class: 'system-status-icon',
            });
            this.add_child(this._icon);

            this._daemon = new CastDaemon({
                onDevicesChanged: () => this._refreshDevices(),
                onStateChanged: (state, deviceId) => this._setState(state, deviceId),
                onError: message => this._notifyError(message),
            });

            this._buildMenu();

            this.menu.connect('open-state-changed', (_menu, open) => {
                if (open)
                    this._refresh();
            });
        }

        _buildMenu() {
            this._devicesSection = new PopupMenu.PopupMenuSection();
            this.menu.addMenuItem(this._devicesSection);

            this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

            this._stopItem = new PopupMenu.PopupMenuItem('Stop Casting');
            this._stopItem.connect('activate', () => this._daemon.stopCast());
            this._stopItem.visible = false;
            this.menu.addMenuItem(this._stopItem);

            const prefsItem = new PopupMenu.PopupMenuItem('Preferences');
            prefsItem.connect('activate', () => this._extension.openPreferences());
            this.menu.addMenuItem(prefsItem);

            this._rebuildDeviceItems();
        }

        _refresh() {
            this._refreshDevices();
            this._daemon.getStatus((state, deviceId) => this._setState(state, deviceId));
        }

        _refreshDevices() {
            this._daemon.listDevices(devices => {
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

            for (const device of this._devices) {
                const item = new PopupMenu.PopupSubMenuMenuItem(device.name, false);

                const screenItem = new PopupMenu.PopupMenuItem('Cast Screen');
                screenItem.connect('activate', () => this._startCast(device, SOURCE_SCREEN));
                item.menu.addMenuItem(screenItem);

                const windowItem = new PopupMenu.PopupMenuItem('Cast Window');
                windowItem.connect('activate', () => this._startCast(device, SOURCE_WINDOW));
                item.menu.addMenuItem(windowItem);

                this._devicesSection.addMenuItem(item);
            }
        }

        _startCast(device, source) {
            this._daemon.startCast(device.id, source, this._castOptions());
        }

        _castOptions() {
            const options = {
                'fps': new GLib.Variant('i', this._settings.get_int('fps')),
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
            this._state = state;
            this._activeDeviceId = deviceId;

            const casting = state === 'casting' || state === 'connecting';
            this._icon.gicon = casting ? this._iconActive : this._iconIdle;
            this._stopItem.visible = casting;

            if (state === 'error')
                this._notifyError('Casting failed, see the daemon logs for details.');
        }

        _notifyError(message) {
            Main.notify('Cast to Chromecast', message);
        }

        destroy() {
            this._daemon.destroy();
            this._daemon = null;
            super.destroy();
        }
    });
