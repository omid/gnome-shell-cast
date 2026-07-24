'use strict';

import Gio from 'gi://Gio';

export const SOURCE_SCREEN = 0;
export const SOURCE_WINDOW = 1;
export const SOURCE_AUDIO = 2;

const BUS_NAME = 'org.gnome.ShellCast';
const OBJECT_PATH = '/org/gnome/ShellCast';

const CAST_IFACE_XML = `
<node>
  <interface name="org.gnome.ShellCast1">
    <method name="ListDevices">
      <arg type="a(sssu)" direction="out" name="devices"/>
    </method>
    <method name="GetStatus">
      <arg type="s" direction="out" name="state"/>
      <arg type="s" direction="out" name="device_id"/>
    </method>
    <method name="GetDetails">
      <arg type="s" direction="out" name="transport"/>
      <arg type="s" direction="out" name="codec"/>
      <arg type="as" direction="out" name="receiver_codecs"/>
    </method>
    <method name="GetLastEvent">
      <arg type="s" direction="out" name="kind"/>
      <arg type="s" direction="out" name="message"/>
    </method>
    <method name="GetVersion">
      <arg type="s" direction="out" name="version"/>
    </method>
    <method name="StartCast">
      <arg type="s" direction="in" name="device_id"/>
      <arg type="u" direction="in" name="source"/>
      <arg type="a{sv}" direction="in" name="options"/>
    </method>
    <method name="StopCast"/>
    <method name="GetVolume">
      <arg type="d" direction="out" name="level"/>
    </method>
    <method name="SetVolume">
      <arg type="d" direction="in" name="level"/>
    </method>
    <signal name="DevicesChanged"/>
    <signal name="StateChanged">
      <arg type="s" name="state"/>
      <arg type="s" name="device_id"/>
    </signal>
    <signal name="VolumeChanged">
      <arg type="d" name="level"/>
    </signal>
  </interface>
</node>`;

const CastProxy = Gio.DBusProxy.makeProxyWrapper(CAST_IFACE_XML);

/**
 * Thin wrapper around the org.gnome.ShellCast1 D-Bus service provided by
 * gnome-shell-cast-daemon. The daemon is D-Bus activatable: constructing the
 * proxy does not launch it, but any method call does.
 */
export class CastDaemon {
    constructor({
        onDevicesChanged,
        onStateChanged,
        onVolumeChanged,
        onDaemonGone,
        onError,
        onStartError,
    }) {
        this._onDevicesChanged = onDevicesChanged;
        this._onStateChanged = onStateChanged;
        this._onVolumeChanged = onVolumeChanged;
        this._onDaemonGone = onDaemonGone;
        this._onError = onError;
        this._onStartError = onStartError;
        this._signalIds = [];

        // Aborts in-flight calls on destroy(); D-Bus replies can outlive
        // disable() and touch already-destroyed widgets.
        this._cancellable = new Gio.Cancellable();

        this._proxy = new CastProxy(
            Gio.DBus.session,
            BUS_NAME,
            OBJECT_PATH,
            (proxy, error) => {
                // Cancelling init_async still invokes this callback, with a
                // "cancelled" error we must not report as a daemon failure.
                if (this._cancellable.is_cancelled()) return;
                if (error) {
                    this._onError?.(error.message);
                    return;
                }
                this._signalIds.push([
                    proxy,
                    proxy.connectSignal('DevicesChanged', () => this._onDevicesChanged?.()),
                ]);
                this._signalIds.push([
                    proxy,
                    proxy.connectSignal('StateChanged', (_p, _sender, [state, deviceId]) =>
                        this._onStateChanged?.(state, deviceId),
                    ),
                ]);
                this._signalIds.push([
                    proxy,
                    proxy.connectSignal('VolumeChanged', (_p, _sender, [level]) =>
                        this._onVolumeChanged?.(level),
                    ),
                ]);
            },
            this._cancellable,
            Gio.DBusProxyFlags.DO_NOT_AUTO_START_AT_CONSTRUCTION |
                Gio.DBusProxyFlags.DO_NOT_LOAD_PROPERTIES,
        );

        // A dying daemon (crash, kill) sends no final StateChanged, leaving the
        // indicator stuck "casting". `onVanished` fires when its bus name loses
        // its owner; it also fires once at startup (handler is a no-op then).
        // Watching does not activate the daemon.
        this._watchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            null,
            () => this._onDaemonGone?.(),
        );
    }

    // Drops a reply handler once destroy() has cancelled — a cancelled call
    // still fires its callback, into now-destroyed menu items.
    _reply(handler) {
        return (...args) => {
            if (!this._cancellable.is_cancelled()) handler(...args);
        };
    }

    listDevices(callback) {
        this._proxy?.ListDevicesRemote(
            this._reply((result, error) => {
                if (error) {
                    // A transient failure (e.g. the daemon still activating just
                    // after login) yields an empty list and the "Searching…"
                    // placeholder; a genuinely missing daemon is reported by the
                    // version check, so don't also raise a notification here.
                    callback([]);
                    return;
                }
                const [devices] = result;
                // Bit 0 of the Cast capability mask = video out; devices without
                // it (speakers, cast groups) can only receive audio.
                callback(
                    devices.map(([id, name, address, capabilities]) => ({
                        id,
                        name,
                        address,
                        hasVideo: (capabilities & 1) !== 0,
                    })),
                );
            }),
            this._cancellable,
        );
    }

    // `noAutoStart` queries status without D-Bus-activating an idle daemon.
    getStatus(callback, { noAutoStart = false } = {}) {
        const reply = this._reply((result, error) => {
            if (error) {
                callback('idle', '');
                return;
            }
            const [state, deviceId] = result;
            callback(state, deviceId);
        });
        // The generated *Remote wrapper reads a trailing number as call flags.
        if (noAutoStart) {
            this._proxy?.GetStatusRemote(reply, Gio.DBusCallFlags.NO_AUTO_START, this._cancellable);
        } else {
            this._proxy?.GetStatusRemote(reply, this._cancellable);
        }
    }

    getDetails(callback) {
        this._proxy?.GetDetailsRemote(
            this._reply((result, error) => {
                if (error) {
                    callback(null);
                    return;
                }
                const [transport, codec, receiverCodecs] = result;
                callback({ transport, codec, receiverCodecs });
            }),
            this._cancellable,
        );
    }

    getLastEvent(callback) {
        this._proxy?.GetLastEventRemote(
            this._reply((result, error) => {
                if (error) {
                    callback({ kind: '', message: '' });
                    return;
                }
                const [kind, message] = result;
                callback({ kind, message });
            }),
            this._cancellable,
        );
    }

    /**
     * Fetches the running daemon's version. Passes null to the callback when
     * the daemon cannot be reached (e.g. it is not installed) - a D-Bus method
     * call auto-starts the daemon, so an error here means activation failed.
     */
    getVersion(callback) {
        this._proxy?.GetVersionRemote(
            this._reply((result, error) => {
                if (error) {
                    callback(null);
                    return;
                }
                callback(result[0]);
            }),
            this._cancellable,
        );
    }

    startCast(deviceId, source, options) {
        this._proxy?.StartCastRemote(
            deviceId,
            source,
            options,
            this._reply((_result, error) => {
                if (error) (this._onStartError ?? this._onError)?.(error.message);
            }),
            this._cancellable,
        );
    }

    stopCast() {
        this._proxy?.StopCastRemote(
            this._reply((_result, error) => {
                if (error) this._onError?.(error.message);
            }),
            this._cancellable,
        );
    }

    getVolume(callback) {
        this._proxy?.GetVolumeRemote(
            this._reply((result, error) => {
                if (error) {
                    callback(null);
                    return;
                }
                callback(result[0]);
            }),
            this._cancellable,
        );
    }

    setVolume(level) {
        this._proxy?.SetVolumeRemote(
            level,
            this._reply((_result, error) => {
                if (error) this._onError?.(error.message);
            }),
            this._cancellable,
        );
    }

    destroy() {
        // Cancel first: aborts in-flight calls and makes _reply() drop queued ones.
        this._cancellable.cancel();
        for (const [proxy, id] of this._signalIds) proxy.disconnectSignal(id);
        this._signalIds = [];
        if (this._watchId) {
            Gio.bus_unwatch_name(this._watchId);
            this._watchId = 0;
        }
        this._proxy = null;
        this._onDevicesChanged = null;
        this._onStateChanged = null;
        this._onVolumeChanged = null;
        this._onDaemonGone = null;
        this._onError = null;
        this._onStartError = null;
    }
}
