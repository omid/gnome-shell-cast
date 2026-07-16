'use strict';

import GObject from 'gi://GObject';

import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';

import { CastMenu, loadIcons } from './castMenu.js';

const CastToggle = GObject.registerClass(
    class CastToggle extends QuickSettings.QuickMenuToggle {
        _init(extension, icons) {
            super._init({
                title: 'Cast',
                gicon: icons.idle,
                toggleMode: false,
            });

            this.menu.setHeader(icons.idle, 'GNOME Shell Cast');

            this._cast = new CastMenu({
                extension,
                menu: this.menu,
                icons,
                setIcon: (active) => {
                    this.gicon = active ? icons.active : icons.idle;
                    this.checked = active;
                },
            });

            // The toggle area itself only makes sense as a quick "stop"; picking
            // a device to cast to happens in the menu opened via the arrow.
            this.connect('clicked', () => {
                if (this._cast.casting) this._cast.stopCast();
            });

            this.menu.connect('open-state-changed', (_menu, open) => {
                if (open) this._cast.refresh();
            });
        }

        destroy() {
            this._cast.destroy();
            this._cast = null;
            super.destroy();
        }
    },
);

export const CastQuickIndicator = GObject.registerClass(
    class CastQuickIndicator extends QuickSettings.SystemIndicator {
        _init(extension) {
            super._init();

            const icons = loadIcons(extension);
            this._indicatorIcon = this._addIndicator();
            this._indicatorIcon.gicon = icons.active;
            // Only take up space in the top bar while actually casting.
            this._indicatorIcon.visible = false;

            this._toggle = new CastToggle(extension, icons);
            this._checkedId = this._toggle.connect('notify::checked', () => {
                this._indicatorIcon.visible = this._toggle.checked;
            });

            this.quickSettingsItems.push(this._toggle);
        }

        destroy() {
            this._toggle.disconnect(this._checkedId);
            this.quickSettingsItems.forEach((item) => item.destroy());
            super.destroy();
        }
    },
);
