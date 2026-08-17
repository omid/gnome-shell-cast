'use strict';

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';
import Gio from 'gi://Gio';

import {
    ExtensionPreferences,
    gettext as _,
} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

const RESOLUTION_VALUES = ['native', '2160', '1440', '1080', '720'];
const LOCATION_VALUES = ['tray', 'quick-settings'];
const ENCODER_VALUES = ['auto', 'hardware', 'software'];
const FORMAT_VALUES = ['auto', 'nv12', 'i420'];

export default class GnomeShellCastPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        this._addGeneralPage(window, settings);
        this._addVideoPage(window, settings);
        this._addAboutPage(window);
    }

    _addVideoPage(window, settings) {
        // Built here (not at module scope) so each label is a literal `_()`
        // call that xgettext can extract and the gettext domain is bound.
        const resolutionLabels = [_('Native'), _('4K (2160p)'), _('1440p'), _('1080p'), _('720p')];
        const encoderLabels = [_('Automatic'), _('Hardware only'), _('Software only')];
        const formatLabels = [_('Automatic'), 'NV12', 'I420'];

        const page = new Adw.PreferencesPage({
            title: _('Video'),
            icon_name: 'video-display-symbolic',
        });
        window.add(page);

        const group = new Adw.PreferencesGroup({
            title: _('Stream quality'),
            description: _('Applied the next time a cast is started'),
        });
        page.add(group);

        const resolutionRow = new Adw.ComboRow({
            title: _('Maximum resolution'),
            subtitle: _('Above 1080p needs a hardware encoder and a matching bitrate'),
            model: new Gtk.StringList({ strings: resolutionLabels }),
            selected: RESOLUTION_VALUES.indexOf(settings.get_string('resolution')),
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
            subtitle: _('kbit/s: about 4000 for 720p, 8000 for 1080p, 30000 for 4K'),
            adjustment: new Gtk.Adjustment({
                lower: 1000,
                upper: 60000,
                step_increment: 500,
            }),
        });
        settings.bind('bitrate-kbps', bitrateRow, 'value', Gio.SettingsBindFlags.DEFAULT);
        group.add(bitrateRow);

        const encodingGroup = new Adw.PreferencesGroup({
            title: _('Encoding'),
            description: _('Casting fails with a message when a forced choice cannot be used'),
        });
        page.add(encodingGroup);

        const encoderRow = new Adw.ComboRow({
            title: _('Video encoder'),
            subtitle: _(
                'Automatic prefers your graphics card; choose software if the picture breaks up',
            ),
            model: new Gtk.StringList({ strings: encoderLabels }),
            selected: ENCODER_VALUES.indexOf(settings.get_string('video-encoder')),
        });
        encoderRow.connect('notify::selected', (row) => {
            settings.set_string('video-encoder', ENCODER_VALUES[row.selected]);
        });
        encodingGroup.add(encoderRow);

        const formatRow = new Adw.ComboRow({
            title: _('Pixel format'),
            subtitle: _('Automatic suits every encoder; only change this to work around a driver'),
            model: new Gtk.StringList({ strings: formatLabels }),
            selected: FORMAT_VALUES.indexOf(settings.get_string('video-format')),
        });
        formatRow.connect('notify::selected', (row) => {
            settings.set_string('video-format', FORMAT_VALUES[row.selected]);
        });
        encodingGroup.add(formatRow);
    }

    _addGeneralPage(window, settings) {
        const locationLabels = [_('Top bar'), _('Quick settings')];

        const page = new Adw.PreferencesPage({
            title: _('General'),
            icon_name: 'preferences-system-symbolic',
        });
        window.add(page);

        const menuGroup = new Adw.PreferencesGroup({ title: _('Menu') });
        page.add(menuGroup);

        const locationRow = new Adw.ComboRow({
            title: _('Indicator location'),
            subtitle: _('Show the cast icon in the top bar, or in the quick settings menu'),
            model: new Gtk.StringList({ strings: locationLabels }),
            selected: LOCATION_VALUES.indexOf(settings.get_string('indicator-location')),
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
    }

    _addAboutPage(window) {
        const url = this.metadata.url;

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
                subtitle: _('Version %s').replace('%s', `${this.metadata.version}.0.0`),
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

        const help = new Adw.PreferencesGroup({
            title: _('Help'),
            description: _('Common problems and their fixes'),
        });
        page.add(help);
        help.add(linkRow(_('Troubleshooting guide'), `${url}/blob/main/TROUBLESHOOTING.md`));
    }
}
