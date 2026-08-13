'use strict';

import GLib from 'gi://GLib';

// Throttles a volume slider's drags into D-Bus writes, and applies
// daemon-reported values back without echoing them. Shared by the
// quick-settings QuickSlider and the top-bar menu's slider row.
export class CastVolumeControl {
    constructor(slider, onChange) {
        this._slider = slider;
        this._onChange = onChange;
        this._fromDaemon = false;
        this._throttleId = 0;
        this._pending = 0;
        this._lastSent = -1;
        this._changedId = slider.connect('notify::value', () => this._onUserChanged());
    }

    // Relies on `notify::value` firing synchronously (St's slider does) so the
    // guard is still set when `_onUserChanged` runs.
    setFromDaemon(level) {
        this._fromDaemon = true;
        this._slider.value = level;
        this._lastSent = level;
        this._fromDaemon = false;
    }

    _onUserChanged() {
        if (this._fromDaemon) return;
        this._pending = this._slider.value;
        // Leading edge: apply the first move at once, then let the timer
        // rate-limit the rest of the drag.
        if (this._throttleId) return;

        this._send();
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
        this._onChange(this._pending);
    }

    destroy() {
        if (this._throttleId) {
            GLib.source_remove(this._throttleId);
            this._throttleId = 0;
        }
        if (this._changedId) {
            this._slider.disconnect(this._changedId);
            this._changedId = 0;
        }
    }
}
