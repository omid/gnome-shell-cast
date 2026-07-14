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
    <method name="GetVersion">
      <arg type="s" direction="out" name="version"/>
    </method>
    <method name="StartCast">
      <arg type="s" direction="in" name="device_id"/>
      <arg type="u" direction="in" name="source"/>
      <arg type="a{sv}" direction="in" name="options"/>
    </method>
    <method name="StopCast"/>
    <signal name="DevicesChanged"/>
    <signal name="StateChanged">
      <arg type="s" name="state"/>
      <arg type="s" name="device_id"/>
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
    constructor({ onDevicesChanged, onStateChanged, onError }) {
        this._onDevicesChanged = onDevicesChanged;
        this._onStateChanged = onStateChanged;
        this._onError = onError;
        this._signalIds = [];

        this._proxy = new CastProxy(
            Gio.DBus.session,
            BUS_NAME,
            OBJECT_PATH,
            (proxy, error) => {
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
                        this._onStateChanged?.(state, deviceId)),
                ]);
            },
            null,
            Gio.DBusProxyFlags.DO_NOT_AUTO_START_AT_CONSTRUCTION |
                Gio.DBusProxyFlags.DO_NOT_LOAD_PROPERTIES,
        );
    }

    listDevices(callback) {
        this._proxy.ListDevicesRemote((result, error) => {
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
            callback(devices.map(([id, name, address, capabilities]) =>
                ({ id, name, address, hasVideo: (capabilities & 1) !== 0 })));
        });
    }

    getStatus(callback) {
        this._proxy.GetStatusRemote((result, error) => {
            if (error) {
                callback('idle', '');
                return;
            }
            const [state, deviceId] = result;
            callback(state, deviceId);
        });
    }

    getDetails(callback) {
        this._proxy.GetDetailsRemote((result, error) => {
            if (error) {
                callback(null);
                return;
            }
            const [transport, codec, receiverCodecs] = result;
            callback({ transport, codec, receiverCodecs });
        });
    }

    /**
     * Fetches the running daemon's version. Passes null to the callback when
     * the daemon cannot be reached (e.g. it is not installed) — a D-Bus method
     * call auto-starts the daemon, so an error here means activation failed.
     */
    getVersion(callback) {
        this._proxy.GetVersionRemote((result, error) => {
            if (error) {
                callback(null);
                return;
            }
            callback(result[0]);
        });
    }

    startCast(deviceId, source, options) {
        this._proxy.StartCastRemote(deviceId, source, options, (_result, error) => {
            if (error)
                this._onError?.(error.message);
        });
    }

    stopCast() {
        this._proxy.StopCastRemote((_result, error) => {
            if (error)
                this._onError?.(error.message);
        });
    }

    destroy() {
        for (const [proxy, id] of this._signalIds)
            proxy.disconnectSignal(id);
        this._signalIds = [];
        this._proxy = null;
    }
}
