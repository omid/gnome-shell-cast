'use strict';

import GObject from 'gi://GObject';
import Pango from 'gi://Pango';
import St from 'gi://St';

import * as Dialog from 'resource:///org/gnome/shell/ui/dialog.js';
import * as ModalDialog from 'resource:///org/gnome/shell/ui/modalDialog.js';

export const BaseDialog = GObject.registerClass(
    class BaseDialog extends ModalDialog.ModalDialog {
        _init({ title, description, content, contentStyle = 'gsc-setup-command' }) {
            super._init({ styleClass: 'gsc-setup-dialog' });

            this.contentLayout.add_child(new Dialog.MessageDialogContent({ title, description }));

            if (content) {
                const label = new St.Label({
                    style_class: contentStyle,
                    text: content,
                });
                label.clutter_text.selectable = true;
                label.clutter_text.line_wrap = true;
                label.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
                this.contentLayout.add_child(label);
            }

            this._status = new St.Label({
                style_class: 'gsc-setup-status',
            });
            this.contentLayout.add_child(this._status);
        }

        setStatusText(text) {
            this._status.text = text;
        }

        addActionButton(label, action, options = {}) {
            this.addButton({
                label,
                action,
                ...options,
            });
        }
    },
);
