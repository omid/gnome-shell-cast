'use strict';

import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';
import Gio from 'gi://Gio';

import {
    ExtensionPreferences,
    gettext as _,
} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import { getEncodingSupport } from './lib/daemon.js';

const RESOLUTION_VALUES = ['auto', 'native', '2160', '1440', '1080', '720'];
// 0 is "automatic" for each of these: the daemon then takes the value from the
// receiver's own limits instead of the setting.
const FPS_VALUES = [0, 15, 20, 24, 30, 60];
const BITRATE_VALUES = [0, 2000, 4000, 8000, 16000, 30000];
const AUDIO_BITRATE_VALUES = [0, 64, 96, 128, 192, 256];
const LOCATION_VALUES = ['tray', 'quick-settings'];
const ENCODER_VALUES = ['auto', 'hardware', 'software', 'vaapi', 'nvenc', 'v4l2'];
const FORMAT_VALUES = ['auto', 'nv12', 'i420'];

// The daemon decides which piece is missing; this only phrases it, because the
// wording has to go through gettext and the daemon's strings do not. A token we
// do not know means silence rather than a guess: the daemon can be a different
// version than the extension reading it.
function hardwareHintText(support) {
    switch (support.gap) {
        case 'driver':
            return _(
                'No VA-API encoder for your graphics card. Install your distribution’s VA-API ' +
                    'driver.',
            );
        case 'nvidia':
            return _('Install the NVIDIA driver and the GStreamer nvcodec plugin.');
        case 'plugin':
            return support.pluginPackage
                ? _('The GStreamer VA-API plugin is missing. Install the %s package.').replace(
                      '%s',
                      support.pluginPackage,
                  )
                : _('The GStreamer VA-API plugin is missing. Install it from your distribution.');
        default:
            return null;
    }
}

export default class GnomeShellCastPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();

        this._addGeneralPage(window, settings);
        this._addAdvancedPage(window, settings);
        this._addAboutPage(window);
    }

    _addAdvancedPage(window, settings) {
        // Built here (not at module scope) so each label is a literal `_()`
        // call that xgettext can extract and the gettext domain is bound.
        const resolutionLabels = [
            _('Automatic'),
            _('Native'),
            _('4K (2160p)'),
            _('1440p'),
            _('1080p'),
            _('720p'),
        ];
        const numericLabels = (values) =>
            values.map((value) => (value === 0 ? _('Automatic') : String(value)));
        const page = new Adw.PreferencesPage({
            title: _('Advanced'),
            icon_name: 'applications-engineering-symbolic',
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
            selected: Math.max(0, RESOLUTION_VALUES.indexOf(settings.get_string('resolution'))),
        });
        resolutionRow.connect('notify::selected', (row) => {
            settings.set_string('resolution', RESOLUTION_VALUES[row.selected]);
        });
        group.add(resolutionRow);

        const fpsRow = new Adw.ComboRow({
            title: _('Framerate'),
            subtitle: _('Frames per second'),
            model: new Gtk.StringList({ strings: numericLabels(FPS_VALUES) }),
            selected: Math.max(0, FPS_VALUES.indexOf(settings.get_int('fps'))),
        });
        fpsRow.connect('notify::selected', (row) => {
            settings.set_int('fps', FPS_VALUES[row.selected]);
        });
        group.add(fpsRow);

        const bitrateRow = new Adw.ComboRow({
            title: _('Video bitrate'),
            subtitle: _('kbit/s: about 4000 for 720p, 8000 for 1080p, 30000 for 4K'),
            model: new Gtk.StringList({ strings: numericLabels(BITRATE_VALUES) }),
            selected: Math.max(0, BITRATE_VALUES.indexOf(settings.get_int('bitrate-kbps'))),
        });
        bitrateRow.connect('notify::selected', (row) => {
            settings.set_int('bitrate-kbps', BITRATE_VALUES[row.selected]);
        });
        group.add(bitrateRow);

        const audioBitrateRow = new Adw.ComboRow({
            title: _('Audio bitrate'),
            model: new Gtk.StringList({ strings: numericLabels(AUDIO_BITRATE_VALUES) }),
            selected: Math.max(
                0,
                AUDIO_BITRATE_VALUES.indexOf(settings.get_int('audio-bitrate-kbps')),
            ),
        });
        audioBitrateRow.connect('notify::selected', (row) => {
            settings.set_int('audio-bitrate-kbps', AUDIO_BITRATE_VALUES[row.selected]);
        });
        group.add(audioBitrateRow);

        this._addEncodingGroup(window, page, settings);

        this._addCastDetailsGroup(page, settings);
    }

    _addEncodingGroup(window, page, settings) {
        // The API choices matter on a machine with more than one: automatic
        // always prefers VA-API, which is not always the better encoder there.
        const encoderLabels = [
            _('Automatic'),
            _('Hardware only'),
            _('Software only'),
            _('VA-API (Intel, AMD)'),
            _('NVENC (NVIDIA)'),
            _('V4L2 (Arm boards)'),
        ];
        const formatLabels = [_('Automatic'), 'NV12', 'I420'];

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
            selected: Math.max(0, ENCODER_VALUES.indexOf(settings.get_string('video-encoder'))),
        });
        encoderRow.connect('notify::selected', (row) => {
            settings.set_string('video-encoder', ENCODER_VALUES[row.selected]);
        });
        encodingGroup.add(encoderRow);

        const formatRow = new Adw.ComboRow({
            title: _('Pixel format'),
            subtitle: _('Automatic suits every encoder; only change this to work around a driver'),
            model: new Gtk.StringList({ strings: formatLabels }),
            selected: Math.max(0, FORMAT_VALUES.indexOf(settings.get_string('video-format'))),
        });
        formatRow.connect('notify::selected', (row) => {
            settings.set_string('video-format', FORMAT_VALUES[row.selected]);
        });
        encodingGroup.add(formatRow);
        this._addHardwareHint(window, encodingGroup);
    }

    // The daemon reports why hardware encoding is unavailable, and says nothing
    // when it works or when there is no graphics card to use - so an empty gap
    // means no row. Added once the daemon answers.
    _addHardwareHint(window, group) {
        const cancellable = new Gio.Cancellable();
        window.connect('destroy', () => cancellable.cancel());

        // Which plugin and driver a card needs, per vendor and per distribution,
        // is more than a row can hold - so the row opens that table instead.
        const guide = 'TROUBLESHOOTING.md#hardware-encoding-by-graphics-card';
        const uri = `${this.metadata.url}/blob/main/${guide}`;

        getEncodingSupport((support) => {
            const subtitle = support && hardwareHintText(support);
            if (!subtitle) return;
            const row = new Adw.ActionRow({
                title: _('Hardware encoding is unavailable'),
                subtitle,
                subtitle_lines: 0,
                activatable: true,
            });
            row.add_prefix(new Gtk.Image({ icon_name: 'dialog-warning-symbolic' }));
            row.add_suffix(new Gtk.Image({ icon_name: 'adw-external-link-symbolic' }));
            row.connect('activated', () => Gio.AppInfo.launch_default_for_uri(uri, null));
            group.add(row);
        }, cancellable);
    }

    _addCastDetailsGroup(page, settings) {
        const menuGroup = new Adw.PreferencesGroup({ title: _('Menu') });
        page.add(menuGroup);

        const detailsRow = new Adw.SwitchRow({
            title: _('Show cast details'),
            subtitle: _('Show the transport and codecs under the active device while casting'),
        });
        settings.bind('show-details', detailsRow, 'active', Gio.SettingsBindFlags.DEFAULT);
        menuGroup.add(detailsRow);
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
