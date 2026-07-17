'use strict';

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';
import Gio from 'gi://Gio';

import {
    ExtensionPreferences,
    gettext as _,
} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const RESOLUTION_VALUES = ['native', '1080', '720'];
const RESOLUTION_LABELS = ['Native', '1080p', '720p'];

const LOCATION_VALUES = ['tray', 'quick-settings'];
const LOCATION_LABELS = ['Top bar', 'Quick settings'];

export default class GnomeShellCastPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        const page = new Adw.PreferencesPage({
            title: _('Settings'),
            icon_name: 'preferences-system-symbolic',
        });
        window.add(page);

        const group = new Adw.PreferencesGroup({
            title: _('Stream quality'),
            description: _('Applied the next time a cast is started'),
        });
        page.add(group);

        const resolutionRow = new Adw.ComboRow({
            title: _('Maximum resolution'),
            subtitle: _('Lower this if playback stutters on your network'),
            model: new Gtk.StringList({ strings: RESOLUTION_LABELS.map((s) => _(s)) }),
            selected: Math.max(0, RESOLUTION_VALUES.indexOf(settings.get_string('resolution'))),
        });
        resolutionRow.connect('notify::selected', (row) => {
            settings.set_string('resolution', RESOLUTION_VALUES[row.selected]);
        });
        group.add(resolutionRow);

        const fpsRow = new Adw.SpinRow({
            title: _('Framerate'),
            subtitle: _('Frames per second'),
            adjustment: new Gtk.Adjustment({
                lower: 10,
                upper: 60,
                step_increment: 5,
            }),
        });
        settings.bind('fps', fpsRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(fpsRow);

        const bitrateRow = new Adw.SpinRow({
            title: _('Video bitrate'),
            subtitle: _('kbit/s'),
            adjustment: new Gtk.Adjustment({
                lower: 1000,
                upper: 20000,
                step_increment: 500,
            }),
        });
        settings.bind('bitrate-kbps', bitrateRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(bitrateRow);

        const menuGroup = new Adw.PreferencesGroup({ title: _('Menu') });
        page.add(menuGroup);

        const locationRow = new Adw.ComboRow({
            title: _('Indicator location'),
            subtitle: _('Show the cast icon in the top bar, or in the quick settings menu'),
            model: new Gtk.StringList({ strings: LOCATION_LABELS.map((s) => _(s)) }),
            selected: Math.max(
                0,
                LOCATION_VALUES.indexOf(settings.get_string('indicator-location')),
            ),
        });
        locationRow.connect('notify::selected', (row) => {
            settings.set_string('indicator-location', LOCATION_VALUES[row.selected]);
        });
        menuGroup.add(locationRow);

        const detailsRow = new Adw.SwitchRow({
            title: _('Show cast details'),
            subtitle: _('Show the transport and codecs under the active device while casting'),
        });
        settings.bind('show-details', detailsRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        menuGroup.add(detailsRow);

        this._addAboutPage(window);
    }

    _addAboutPage(window) {
        const url = this.metadata.url ?? 'https://github.com/omid/gnome-shell-cast';

        const page = new Adw.PreferencesPage({
            title: _('About'),
            icon_name: 'help-about-symbolic',
        });
        window.add(page);

        const group = new Adw.PreferencesGroup();
        page.add(group);

        group.add(
            new Adw.ActionRow({
                title: this.metadata.name,
                subtitle: _('Version %s').replace('%s', String(this.metadata.version)),
            }),
        );

        const linkRow = (title, uri) => {
            const row = new Adw.ActionRow({ title, subtitle: uri, activatable: true });
            row.add_suffix(new Gtk.Image({ icon_name: 'adw-external-link-symbolic' }));
            row.connect('activated', () => Gio.AppInfo.launch_default_for_uri(uri, null));
            return row;
        };

        group.add(linkRow(_('Homepage'), url));
        group.add(linkRow(_('Report an issue'), `${url}/issues`));
    }
}
