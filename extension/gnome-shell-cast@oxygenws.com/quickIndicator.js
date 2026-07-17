'use strict';

import GObject from 'gi://GObject';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { CastMenu, loadIcons } from './castMenu.js';
import { CastVolumeControl } from './volumeControl.js';

// Volume slider for the active cast device, shown among the Quick Settings
// volume sliders while casting. Moving it sets the receiver's volume via the
// daemon, which reports it back to keep the slider in sync.
const CastVolumeSlider = GObject.registerClass(
    class CastVolumeSlider extends QuickSettings.QuickSlider {
        _init(gicon, onChange) {
            super._init({ gicon });
            // Hidden until a cast is active.
            this.visible = false;
            this._control = new CastVolumeControl(this.slider, onChange);
        }

        setCasting(casting, deviceName) {
            this.visible = casting;
            if (casting) {
                this.accessible_name = _('%s volume').replace('%s', deviceName);
                // Position now that the item is in the grid (a cast is active).
                this._placeInVolumeSection();
            }
        }

        setValueFromDaemon(level) {
            this._control.setFromDaemon(level);
        }

        // Makes the slider full width and moves it below the system output
        // slider. Reaches into private Quick Settings internals (GNOME 48-50
        // layout), so every step is guarded to degrade to a plain half-width
        // tile rather than throw; width and position fail independently.
        _placeInVolumeSection() {
            const grid = this.get_parent();
            if (!grid) return;

            // Span both columns (external items default to a single column).
            try {
                grid.layout_manager?.child_set_property(grid, this, 'column-span', 2);
            } catch {
                // No column-span child property on this layout; keep the default.
            }

            // Move directly below the system output volume slider, once.
            if (this._positioned) return;
            try {
                const qs = Main.panel.statusArea.quickSettings;
                const output =
                    qs?._volumeOutput?.quickSettingsItems?.[0] ??
                    qs?._volume?._output ??
                    grid
                        .get_children()
                        .find((child) => child.constructor?.name === 'OutputStreamSlider');
                if (output && output.get_parent() === grid) {
                    grid.set_child_above_sibling(this, output);
                    this._positioned = true;
                }
            } catch {
                // Unknown layout; leave the slider where it was added.
            }
        }

        destroy() {
            this._control.destroy();
            super.destroy();
        }
    },
);

const CastToggle = GObject.registerClass(
    class CastToggle extends QuickSettings.QuickMenuToggle {
        _init(extension, icons, hooks) {
            super._init({
                title: _('Cast'),
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
                onCastChanged: hooks.onCastChanged,
                onVolume: hooks.onVolume,
            });

            // Casting: a click is a quick "stop". Idle: nothing to toggle, so
            // open the menu to pick a device.
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
        _init(extension) {
            super._init();

            const icons = loadIcons(extension);
            this._indicatorIcon = this._addIndicator();
            this._indicatorIcon.gicon = icons.active;
            // Shown only while casting; wear the shell's privacy-indicator class
            // for GNOME's orange (the active mic / screen-sharing tint).
            this._indicatorIcon.add_style_class_name('privacy-indicator');
            this._indicatorIcon.visible = false;

            this._slider = new CastVolumeSlider(icons.active, (level) =>
                this._toggle.setVolume(level),
            );

            this._toggle = new CastToggle(extension, icons, {
                onCastChanged: (casting, deviceName) => {
                    this._slider.setCasting(casting, deviceName);
                    // Fetch the current level when a cast begins, in case the
                    // daemon's volume signal arrived before the slider existed.
                    if (casting) {
                        this._toggle.getVolume((level) => {
                            if (level !== null) this._slider.setValueFromDaemon(level);
                        });
                    }
                },
                onVolume: (level) => this._slider.setValueFromDaemon(level),
            });

            this._checkedId = this._toggle.connect('notify::checked', () => {
                this._indicatorIcon.visible = this._toggle.checked;
            });

            this.quickSettingsItems.push(this._toggle);
            this.quickSettingsItems.push(this._slider);
        }

        destroy() {
            this._toggle.disconnect(this._checkedId);
            this.quickSettingsItems.forEach((item) => item.destroy());
            super.destroy();
        }
    },
);
