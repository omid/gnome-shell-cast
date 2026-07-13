'use strict';

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';
import Gio from 'gi://Gio';

import { ExtensionPreferences } from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const RESOLUTION_VALUES = ['native', '1080', '720'];
const RESOLUTION_LABELS = ['Native', '1080p', '720p'];

export default class GnomeShellCastPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        const page = new Adw.PreferencesPage();
        window.add(page);

        const group = new Adw.PreferencesGroup({
            title: 'Stream quality',
            description: 'Applied the next time a cast is started',
        });
        page.add(group);

        const resolutionRow = new Adw.ComboRow({
            title: 'Maximum resolution',
            subtitle: 'Lower this if playback stutters on your network',
            model: new Gtk.StringList({ strings: RESOLUTION_LABELS }),
            selected: Math.max(0, RESOLUTION_VALUES.indexOf(settings.get_string('resolution'))),
        });
        resolutionRow.connect('notify::selected', row => {
            settings.set_string('resolution', RESOLUTION_VALUES[row.selected]);
        });
        group.add(resolutionRow);

        const fpsRow = new Adw.SpinRow({
            title: 'Framerate',
            subtitle: 'Frames per second',
            adjustment: new Gtk.Adjustment({
                lower: 10, upper: 60, step_increment: 5,
            }),
        });
        settings.bind('fps', fpsRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(fpsRow);

        const bitrateRow = new Adw.SpinRow({
            title: 'Video bitrate',
            subtitle: 'kbit/s',
            adjustment: new Gtk.Adjustment({
                lower: 1000, upper: 20000, step_increment: 500,
            }),
        });
        settings.bind('bitrate-kbps', bitrateRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(bitrateRow);
    }
}
