'use strict';

import GLib from 'gi://GLib';

// Drives a cast volume slider, shared by the quick-settings QuickSlider and the
// top-bar menu's slider row. Given a Slider.Slider (anything with a `value`
// property and a `notify::value` signal) and an `onChange(level)` callback, it
// throttles user drags into D-Bus writes and applies daemon-reported values
// back without echoing them. Call destroy() when the slider goes away.
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

    // Reflects the receiver's volume without echoing it back as a change.
    // Relies on `notify::value` firing synchronously (St's slider does) so the
    // `_fromDaemon` guard is still set when `_onUserChanged` runs.
    setFromDaemon(level) {
        this._fromDaemon = true;
        this._slider.value = level;
        this._lastSent = level;
        this._fromDaemon = false;
    }

    _onUserChanged() {
        if (this._fromDaemon) return;
        this._pending = this._slider.value;
        // Leading edge: apply the first move immediately, then rate-limit the
        // stream of updates while dragging so we don't flood D-Bus.
        if (!this._throttleId) {
            this._send();
            this._scheduleFlush();
        }
    }

    _scheduleFlush() {
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
        this._onChange?.(this._pending);
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
