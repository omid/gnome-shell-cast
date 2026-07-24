'use strict';

import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';
import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

// Shown when a cast fails. Displays the daemon's error text and helps the user
// report it: a button that opens a pre-filled GitHub issue and a button that
// copies the details to the clipboard. It never sends anything itself.
export const ErrorDialog = GObject.registerClass(
    class ErrorDialog extends ModalDialog.ModalDialog {
        _init({ message, version, url }) {
            super._init({ styleClass: 'gsc-setup-dialog' });

            this._url = url;
            this._details =
                `${message}\n\nVersion ${version}\n\n` +
                'Please also paste the output of: ' +
                'journalctl --user -g gnome-shell-cast';

            this.contentLayout.add_child(
                new Dialog.MessageDialogContent({
                    title: _('Casting failed'),
                    description: _(
                        'If this keeps happening, please report it (that helps get it fixed).',
                    ),
                }),
            );

            const error = new St.Label({
                style_class: 'gsc-setup-command',
                text: message,
            });
            error.clutter_text.selectable = true;
            error.clutter_text.line_wrap = true;
            error.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
            this.contentLayout.add_child(error);

            this._status = new St.Label({
                style_class: 'gsc-setup-status',
                text: _(
                    'The report will include this error, the version, and a ' +
                        'reminder to attach logs.',
                ),
            });
            this.contentLayout.add_child(this._status);

            this.addButton({
                label: _('Copy details'),
                action: () => this._copy(),
            });
            this.addButton({
                label: _('Close'),
                action: () => this.close(),
                key: Clutter.KEY_Escape,
            });
            this.addButton({
                label: _('Report an issue'),
                action: () => this._report(),
                default: true,
            });
        }

        _copy() {
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, this._details);
            this._status.text = _('Copied! Paste it into a new issue.');
        }

        _report() {
            const query =
                `?title=${encodeURIComponent('Cast error')}` +
                `&body=${encodeURIComponent(this._details)}`;
            Gio.AppInfo.launch_default_for_uri(`${this._url}/issues/new${query}`, null);
        }
    },
);
