'use strict';

import GObject from 'gi://GObject';
import GLib from 'gi://GLib';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';

import { CastMenu, loadIcons } from './castMenu.js';

// A volume slider for the active cast device, shown among the Quick Settings
// volume sliders while casting. Moving it sets the receiver's own volume via
// the daemon; the daemon reports the receiver's volume back so the slider stays
// in sync (initial value and any external change).
const CastVolumeSlider = GObject.registerClass(
    class CastVolumeSlider extends QuickSettings.QuickSlider {
        _init(gicon, onChange) {
            super._init({ gicon });
            this._onChange = onChange;
            this._fromDaemon = false;
            this._throttleId = 0;
            this._pending = 0;
            this._lastSent = -1;

            // Hidden until a cast is active.
            this.visible = false;

            this._changedId = this.slider.connect('notify::value', () => this._onUserChanged());
        }

        // Shows or hides the slider for the active cast and labels it for
        // screen readers with the device name.
        setCasting(casting, deviceName) {
            this.visible = casting;
            if (casting) {
                this.accessible_name = `${deviceName} volume`;
                // Do this once the item is actually in the grid (a cast is now
                // active, so the quick-settings items have been laid out).
                this._placeInVolumeSection();
            }
        }

        // Reflects the receiver's volume without echoing it back as a change.
        setValueFromDaemon(level) {
            this._fromDaemon = true;
            this.slider.value = level;
            this._lastSent = level;
            this._fromDaemon = false;
        }

        // Makes the slider full width (external quick-settings items default to
        // a single, half-width column) and moves it directly below the general
        // system output volume slider, among the native volume controls.
        // Best-effort: leaves defaults in place if the layout can't be matched.
        _placeInVolumeSection() {
            const grid = this.get_parent();
            if (!grid) return;
            // Span both columns like the other volume sliders.
            try {
                grid.layout_manager.child_set_property(grid, this, 'column-span', 2);
            } catch {
                // Layout without a column-span child property; leave as-is.
            }
            if (this._positioned) return;
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
        }

        _onUserChanged() {
            if (this._fromDaemon) return;
            this._pending = this.slider.value;
            // Leading edge: apply the first move immediately, then rate-limit
            // the stream of updates while dragging so we don't flood D-Bus.
            if (!this._throttleId) {
                this._send();
                this._scheduleFlush();
            }
        }

        _scheduleFlush() {
            this._throttleId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 80, () => {
                if (this._pending !== this._lastSent) {
                    this._send();
                    return GLib.SOURCE_CONTINUE;
                }
                this._throttleId = 0;
                return GLib.SOURCE_REMOVE;
            });
        }

        _send() {
            this._lastSent = this._pending;
            this._onChange?.(this._pending);
        }

        destroy() {
            if (this._throttleId) {
                GLib.source_remove(this._throttleId);
                this._throttleId = 0;
            }
            if (this._changedId) {
                this.slider.disconnect(this._changedId);
                this._changedId = 0;
            }
            super.destroy();
        }
    },
);

const CastToggle = GObject.registerClass(
    class CastToggle extends QuickSettings.QuickMenuToggle {
        _init(extension, icons, hooks) {
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
                onCastChanged: hooks.onCastChanged,
                onVolume: hooks.onVolume,
            });

            // The toggle area itself only makes sense as a quick "stop"; picking
            // a device to cast to happens in the menu opened via the arrow.
            this.connect('clicked', () => {
                if (this._cast.casting) this._cast.stopCast();
            });

            this._openStateId = this.menu.connect('open-state-changed', (_menu, open) => {
                if (open) this._cast.refresh();
            });
        }

        setVolume(level) {
            this._cast.setVolume(level);
        }

        getVolume(callback) {
            this._cast.getVolume(callback);
        }

        destroy() {
            if (this._openStateId) {
                this.menu.disconnect(this._openStateId);
                this._openStateId = null;
            }
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
            // This icon only shows while casting, so it always wears the orange
            // "streaming" tint: the shell's own privacy-indicator class (so it
            // follows GNOME's mic / screen-sharing colour, theme-aware) plus a
            // fallback for themes that do not define it.
            this._indicatorIcon.add_style_class_name('gsc-casting-icon');
            this._indicatorIcon.add_style_class_name('privacy-indicator');
            // Only take up space in the top bar while actually casting.
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
