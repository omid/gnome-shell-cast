'use strict';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastPanelIndicator } from './indicator.js';
import { CastQuickIndicator } from './quickIndicator.js';

export default class GnomeShellCastExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._locationId = this._settings.connect('changed::indicator-location', () =>
            this._rebuildIndicator(),
        );
        this._buildIndicator();
    }

    _buildIndicator() {
        if (this._settings.get_string('indicator-location') === 'quick-settings') {
            this._indicator = new CastQuickIndicator(this);
            Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);
        } else {
            this._indicator = new CastPanelIndicator(this);
            Main.panel.addToStatusArea(this.uuid, this._indicator);
        }
    }

    _rebuildIndicator() {
        this._indicator?.destroy();
        this._indicator = null;
        this._buildIndicator();
    }

    disable() {
        this._settings.disconnect(this._locationId);
        this._locationId = null;
        this._settings = null;
        this._indicator?.destroy();
        this._indicator = null;
    }
}
