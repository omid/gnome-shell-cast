'use strict';

import GObject from 'gi://GObject';
import St from 'gi://St';

import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastMenu, loadIcons } from './castMenu.js';

function createIconUpdater(icon, icons) {
    return (active) => {
        icon.gicon = active ? icons.active : icons.idle;
        if (active) icon.add_style_class_name('privacy-indicator');
        else icon.remove_style_class_name('privacy-indicator');
    };
}

export const CastPanelIndicator = GObject.registerClass(
    class CastPanelIndicator extends PanelMenu.Button {
        _init(extension) {
            super._init(0.5, _('GNOME Shell Cast'));

            this._icons = loadIcons(extension);
            this._icon = new St.Icon({
                gicon: this._icons.idle,
                style_class: 'system-status-icon',
            });
            this.add_child(this._icon);

            this._cast = new CastMenu({
                extension,
                menu: this.menu,
                icons: this._icons,
                inlineVolume: true,
                setIcon: createIconUpdater(this._icon, this._icons),
            });
        }

        destroy() {
            this._cast.destroy();
            this._cast = null;
            super.destroy();
        }
    },
);
