'use strict';

import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import St from 'gi://St';

import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { BaseDialog } from './baseDialog.js';

export const SetupDialog = GObject.registerClass(
    class SetupDialog extends BaseDialog {
        constructor({ mode, command, currentVersion, requiredVersion, url }) {
            const isUpdate = mode === 'update';
            const title = isUpdate ? _('Update the cast daemon') : _('Set up the cast daemon');
            const description = isUpdate
                ? _(
                      'A newer version of the extension needs a matching daemon ' +
                          '(installed v%old → needs v%new). ' +
                          'Run the command below to update it (nothing runs as root).',
                  )
                      .replace('%old', currentVersion)
                      .replace('%new', requiredVersion)
                : _(
                      'GNOME Shell Cast needs a small background daemon. It can’t be ' +
                          'shipped through extensions.gnome.org, so install it once with the ' +
                          'command below. It downloads a checksum-verified binary to ' +
                          '~/.local/bin (nothing runs as root).',
                  );

            super({
                title,
                description,
                content: command,
            });

            this._command = command;
            this._url = url;

            this.setStatusText(_('Copy the command, then paste it into a terminal and run it.'));

            this.addActionButton(_('Homepage'), () => this._openInstructions());
            this.addActionButton(_('Close'), () => this.close(), {
                key: Clutter.KEY_Escape,
            });
            this.addActionButton(_('Copy command'), () => this._copy(), {
                default: true,
            });
        }

        _copy() {
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, this._command);
            this.setStatusText(_('Copied! Paste it into a terminal and run it.'));
        }

        _openInstructions() {
            Gio.AppInfo.launch_default_for_uri(this._url, null);
        }
    },
);
