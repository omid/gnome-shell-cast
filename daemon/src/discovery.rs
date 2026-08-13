use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use log::{debug, info, warn};
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};

use crate::{Event, SharedState};

const SERVICE_TYPE: &str = "_googlecast._tcp.local.";

/// Bit 0 of the Cast `ca` TXT capability mask (Chromium
/// `CastDeviceCapability::VIDEO_OUT`). Devices without it - Chromecast Audio,
/// Google/Nest speakers, cast groups - can only receive audio.
const CA_VIDEO_OUT: u32 = 1;

/// Parses the `ca` (capabilities) TXT value. Missing or unparseable values
/// default to video-capable so unknown devices are never hidden or blocked.
fn parse_capabilities(ca: Option<&str>) -> u32 {
    ca.and_then(|s| s.parse().ok()).unwrap_or(CA_VIDEO_OUT)
}

/// Whether the host has a route to `addr`.
fn routable(addr: IpAddr) -> bool {
    crate::net::connected_udp(addr, 9).is_ok()
}

/// Picks the address to reach a device on. Reachability comes first - a
/// device's IPv6 record is useless on an IPv4-only network - then family, then
/// whatever was announced, so a device still shows up if the probe can't help.
fn pick_address(addresses: &[IpAddr]) -> Option<IpAddr> {
    addresses
        .iter()
        .find(|a| a.is_ipv4() && routable(**a))
        .or_else(|| addresses.iter().find(|a| routable(**a)))
        .or_else(|| addresses.iter().find(|a| a.is_ipv4()))
        .or_else(|| addresses.first())
        .copied()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// mDNS fullname; opaque, stable identifier used over D-Bus.
    pub id: String,
    /// Friendly name from the TXT record ("fn"), e.g. "Living Room TV".
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
    /// Effective capability bitmask from the `ca` TXT key (see
    /// [`CA_VIDEO_OUT`]); defaults to video-capable when absent or malformed.
    pub ca: u32,
}

impl Device {
    pub fn has_video(&self) -> bool {
        self.ca & CA_VIDEO_OUT != 0
    }
}

/// Browses for Chromecast devices for the daemon's whole lifetime, keeping
/// `state.devices` up to date and emitting `DevicesChanged` events.
///
/// Discovery is best-effort and runs on its own thread: it never fails the
/// daemon, so a network that isn't up yet right after login only delays the
/// first results instead of aborting D-Bus activation.
pub fn start(state: Arc<SharedState>) {
    if let Err(e) = thread::Builder::new()
        .name("mdns-discovery".into())
        .spawn(move || run(&state))
    {
        warn!("could not start the device discovery thread: {e}");
    }
}

/// Records one resolved service, keeping `state.devices` in step.
fn resolved(
    state: &Arc<SharedState>,
    announced: &mut HashMap<String, Vec<IpAddr>>,
    info: &mdns_sd::ResolvedService,
) {
    let fullname = info.get_fullname();
    let addrs: Vec<IpAddr> = info
        .get_addresses()
        .iter()
        .filter_map(|v| match v {
            ScopedIp::V4(ip) => Some(IpAddr::from(*ip.addr())),
            ScopedIp::V6(ip) => Some(IpAddr::from(*ip.addr())),
            _ => None,
        })
        .collect();

    let addresses = announced.entry(fullname.to_owned()).or_default();
    for addr in addrs.iter().rev() {
        addresses.retain(|a| a != addr);
        addresses.insert(0, *addr);
    }
    debug!("resolved {fullname} with {addrs:?}, known {addresses:?}");
    let Some(addr) = pick_address(addresses) else {
        warn!("resolved {fullname} without addresses");
        return;
    };

    let name = info.get_property_val_str("fn").unwrap_or_else(|| {
        fullname
            .split("._googlecast")
            .next()
            .unwrap_or("Chromecast")
    });
    let port = info.get_port();
    let ca = parse_capabilities(info.get_property_val_str("ca"));

    // Chromecasts re-announce periodically; only build (and log) a Device when
    // something actually changed.
    let changed = {
        let mut devices = state.devices.lock();
        // Never downgrade a working address to an unreachable one.
        let addr = match devices.get(fullname) {
            Some(e) if e.addr != addr && !routable(addr) && routable(e.addr) => {
                debug!("keeping {} at {} over unreachable {addr}", e.name, e.addr);
                e.addr
            }
            _ => addr,
        };
        match devices.get(fullname) {
            Some(e) if e.name == name && e.addr == addr && e.port == port && e.ca == ca => false,
            _ => {
                devices.insert(
                    fullname.to_owned(),
                    Device {
                        id: fullname.to_owned(),
                        name: name.to_owned(),
                        addr,
                        port,
                        ca,
                    },
                );
                true
            }
        }
    };
    if changed {
        info!("found {name} at {addr}:{port}");
        let _ = state.events.send(Event::Devices);
    } else {
        debug!("refreshed {name} at {addr}:{port}");
    }
}

fn run(state: &Arc<SharedState>) {
    // mDNS can be unavailable for a moment after login (no network yet); keep
    // retrying rather than giving up for the rest of the daemon's life. Warn
    // once, then stay quiet so a permanently mDNS-less box isn't spammed.
    let mut warned = false;
    let mdns = loop {
        match ServiceDaemon::new() {
            Ok(mdns) => break mdns,
            Err(e) => {
                if warned {
                    debug!("mDNS still unavailable: {e}");
                } else {
                    warn!("mDNS not available yet, retrying: {e}");
                    warned = true;
                }
                thread::sleep(Duration::from_secs(5));
            }
        }
    };
    let receiver = match mdns.browse(SERVICE_TYPE) {
        Ok(receiver) => receiver,
        Err(e) => {
            warn!("could not browse for Chromecast devices: {e}");
            return;
        }
    };

    // Every address a device has announced, freshest first: one resolve can
    // carry only part of the record (after a resume, often just the AAAA).
    let mut announced: HashMap<String, Vec<IpAddr>> = HashMap::new();

    while let Ok(event) = receiver.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                resolved(state, &mut announced, &info);
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                info!("lost {fullname}");
                announced.remove(&fullname);
                if state.devices.lock().remove(&fullname).is_some() {
                    let _ = state.events.send(Event::Devices);
                }
            }
            // The trailing `_` is required: ServiceEvent is #[non_exhaustive].
            other @ (ServiceEvent::SearchStarted(_)
            | ServiceEvent::ServiceFound(..)
            | ServiceEvent::SearchStopped(_)
            | _) => debug!("mdns event: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_preference() {
        let v4 = IpAddr::from([192, 168, 1, 5]);
        let loopback = IpAddr::from([127, 0, 0, 1]);
        // Documentation range (RFC 3849): announced, never routable.
        let v6: IpAddr = "2001:db8::1".parse().unwrap();

        assert_eq!(pick_address(&[]), None);
        assert_eq!(pick_address(&[v6, loopback]), Some(loopback));
        // Only unreachable addresses: still surface the device.
        assert_eq!(pick_address(&[v6]), Some(v6));
        // Freshest first among equally reachable addresses.
        assert_eq!(pick_address(&[loopback, v4]), Some(loopback));
    }

    #[test]
    fn known_capability_masks() {
        // Video Chromecasts: bit 0 set (classic ca=5, Google TV ca=4101).
        assert_eq!(parse_capabilities(Some("5")) & CA_VIDEO_OUT, 1);
        assert_eq!(parse_capabilities(Some("4101")) & CA_VIDEO_OUT, 1);
        // Chromecast Audio (2052) and cast groups (multizone bit 32): no video.
        assert_eq!(parse_capabilities(Some("2052")) & CA_VIDEO_OUT, 0);
        assert_eq!(parse_capabilities(Some("32")) & CA_VIDEO_OUT, 0);
    }

    #[test]
    fn missing_or_malformed_ca_defaults_to_video() {
        for ca in [
            None,
            Some(""),
            Some("banana"),
            Some("-1"),
            Some("99999999999999999999"),
        ] {
            assert_eq!(parse_capabilities(ca), CA_VIDEO_OUT, "ca = {ca:?}");
        }
    }

    #[test]
    fn device_has_video() {
        let device = |ca| Device {
            id: String::new(),
            name: String::new(),
            addr: IpAddr::from([127, 0, 0, 1]),
            port: 8009,
            ca,
        };
        assert!(device(1).has_video());
        assert!(device(4101).has_video());
        assert!(!device(0).has_video());
        assert!(!device(2052).has_video());
    }
}
