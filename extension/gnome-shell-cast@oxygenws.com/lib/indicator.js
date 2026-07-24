'use strict';

import GObject from 'gi://GObject';
import St from 'gi://St';

import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';

import { CastMenu, loadIcons } from './castMenu.js';

export const CastPanelIndicator = GObject.registerClass(
    class CastPanelIndicator extends PanelMenu.Button {
        _init(extension) {
            super._init(0.5, 'GNOME Shell Cast');

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
                setIcon: (active) => {
                    this._icon.gicon = active ? this._icons.active : this._icons.idle;
                    // Orange (shell privacy-indicator tint) while streaming.
                    if (active) this._icon.add_style_class_name('privacy-indicator');
                    else this._icon.remove_style_class_name('privacy-indicator');
                },
            });
        }

        destroy() {
            this._cast.destroy();
            this._cast = null;
            super.destroy();
        }
    },
);
