import Clutter from 'gi://Clutter';
import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import St from 'gi://St';

import { gettext as _ } from 'resource:///org/gnome/shell/extensions/extension.js';

import { BaseDialog } from './baseDialog.js';

export const ErrorDialog = GObject.registerClass(
    class ErrorDialog extends BaseDialog {
        constructor({ message, version, url }) {
            super({
                title: _('Casting failed'),
                description: _(
                    'If this keeps happening, please report it (that helps get it fixed).',
                ),
                content: message,
            });

            this._url = url;
            this._details =
                `${message}\n\nVersion ${version}\n\n` +
                'Please also paste the output of: ' +
                'journalctl --user -g gnome-shell-cast';

            this.setStatusText(
                _(
                    'The report will include this error, the version, and a ' +
                        'reminder to attach logs.',
                ),
            );

            this.addActionButton(_('Copy details'), () => this._copy());
            this.addActionButton(_('Close'), () => this.close(), {
                key: Clutter.KEY_Escape,
            });
            this.addActionButton(_('Report an issue'), () => this._report(), {
                default: true,
            });
        }

        _copy() {
            St.Clipboard.get_default().set_text(St.ClipboardType.CLIPBOARD, this._details);
            this.setStatusText(_('Copied! Paste it into a new issue.'));
        }

        _report() {
            const query =
                `?title=${encodeURIComponent('Cast error')}` +
                `&body=${encodeURIComponent(this._details)}`;
            Gio.AppInfo.launch_default_for_uri(`${this._url}/issues/new${query}`, null);
        }
    },
);
