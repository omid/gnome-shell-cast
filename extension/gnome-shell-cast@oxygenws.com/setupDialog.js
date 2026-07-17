'use strict';

import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

// Modal shown from the indicator when the daemon is missing (mode 'install')
// or out of date relative to the extension (mode 'update'). It only shows the
// install command and copies it to the clipboard - it never runs anything, so
// the user stays in control and extensions.gnome.org review stays happy.
export const SetupDialog = GObject.registerClass(
    class SetupDialog extends ModalDialog.ModalDialog {
        _init({ mode, command, currentVersion, requiredVersion, url }) {
            super._init({ styleClass: 'gsc-setup-dialog' });

            this._command = command;
            this._url = url;

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

            this.contentLayout.add_child(new Dialog.MessageDialogContent({ title, description }));

            // The command, selectable so it can also be copied by hand.
            const cmd = new St.Label({
                style_class: 'gsc-setup-command',
                text: command,
            });
            cmd.clutter_text.selectable = true;
            cmd.clutter_text.line_wrap = true;
            cmd.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
            this.contentLayout.add_child(cmd);

            this._status = new St.Label({
                style_class: 'gsc-setup-status',
                text: _('Copy the command, then paste it into a terminal and run it.'),
            });
            this.contentLayout.add_child(this._status);

            this.addButton({
                label: _('Homepage'),
                action: () => this._openInstructions(),
            });
            this.addButton({
                label: _('Close'),
                action: () => this.close(),
                key: Clutter.KEY_Escape,
            });
            this.addButton({
                label: _('Copy command'),
                action: () => this._copy(),
                default: true,
            });
        }

        _copy() {
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, this._command);
            this._status.text = _('Copied! Paste it into a terminal and run it.');
        }

        _openInstructions() {
            Gio.AppInfo.launch_default_for_uri(this._url, null);
        }
    },
);
