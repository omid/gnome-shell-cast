import GObject from 'gi://GObject';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastMenu, loadIcons } from './castMenu.js';
import { CastVolumeControl } from './volumeControl.js';

// Volume slider for the active cast device, shown among the Quick Settings
// volume sliders while casting.
const CastVolumeSlider = GObject.registerClass(
    class CastVolumeSlider extends QuickSettings.QuickSlider {
        constructor(gicon, onChange) {
            super({ gicon });
            this.visible = false;
            this._control = new CastVolumeControl(this.slider, onChange);
        }

        setCasting(casting, deviceName) {
            this.visible = casting;
            if (casting) {
                this.accessible_name = _('%s volume').replace('%s', deviceName);
            }
        }

        setValueFromDaemon(level) {
            this._control.setFromDaemon(level);
        }

        destroy() {
            this._control.destroy();
            super.destroy();
        }
    },
);

function createToggleIconUpdater(toggle, icons) {
    return (active) => {
        toggle.gicon = active ? icons.active : icons.idle;
        toggle.checked = active;
    };
}

const CastToggle = GObject.registerClass(
    class CastToggle extends QuickSettings.QuickMenuToggle {
        constructor(extension, settings, icons, hooks) {
            super({
                title: _('Cast'),
                gicon: icons.idle,
                toggleMode: false,
            });

            this.menu.setHeader(icons.idle, _('GNOME Shell Cast'));

            this._cast = new CastMenu({
                extension,
                settings,
                menu: this.menu,
                icons,
                setIcon: createToggleIconUpdater(this, icons),
                onCastChanged: hooks.onCastChanged,
                onVolume: hooks.onVolume,
                // Optional call: losing it only leaves the panel open.
                closeMenu: () => Main.panel.closeQuickSettings?.(),
            });

            this.connect('clicked', () => {
                if (this._cast.casting) this._cast.stopCast();
                else this.menu.open();
            });
        }

        setVolume(level) {
            this._cast.setVolume(level);
        }

        getVolume(callback) {
            this._cast.getVolume(callback);
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
        constructor(extension, settings) {
            super();

            const icons = loadIcons(extension);
            this._indicatorIcon = this._addIndicator();
            this._indicatorIcon.gicon = icons.active;
            // The shell's privacy-indicator class gives GNOME's orange, matching
            // the active mic / screen-sharing tint.
            this._indicatorIcon.add_style_class_name('privacy-indicator');
            this._indicatorIcon.visible = false;

            this._slider = new CastVolumeSlider(icons.active, (level) =>
                this._toggle.setVolume(level),
            );

            this._toggle = new CastToggle(extension, settings, icons, {
                onCastChanged: (casting, deviceName) => {
                    this._slider?.setCasting(casting, deviceName);
                    // In case the daemon's volume signal arrived before the
                    // slider existed.
                    if (casting) {
                        this._toggle.getVolume((level) => {
                            if (level !== null) this._slider?.setValueFromDaemon(level);
                        });
                    }
                },
                onVolume: (level) => this._slider?.setValueFromDaemon(level),
            });

            this._checkedId = this._toggle.connect('notify::checked', () => {
                this._indicatorIcon.visible = this._toggle.checked;
            });

            this.quickSettingsItems.push(this._toggle);

            this._addSlider();
        }

        // Appended full-width. Placing it next to the system volume sliders
        // instead would mean naming one as the sibling for
        // insertItemBefore(), and the shell exposes no public way to.
        //
        // The one shell API we reach into, so it is guarded: a throw here
        // would abort enable() and cost the user the whole extension, not
        // just the slider. (addItem is unchanged across shell 48-50.)
        _addSlider() {
            try {
                Main.panel.statusArea.quickSettings.menu.addItem(this._slider, 2);
            } catch (e) {
                console.warn(`gnome-shell-cast: no volume slider, casting still works: ${e}`);
                this._slider.destroy();
                this._slider = null;
            }
        }

        destroy() {
            this._toggle.disconnect(this._checkedId);
            // Destroyed explicitly because it isn't in quickSettingsItems.
            this._slider?.destroy();
            this.quickSettingsItems.forEach((item) => item.destroy());
            super.destroy();
        }
    },
);
