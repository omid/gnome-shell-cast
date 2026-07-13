use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use log::{debug, info, warn};
use mdns_sd::{ScopedIp, ServiceDaemon, ServiceEvent};

use crate::{Event, SharedState};

const SERVICE_TYPE: &str = "_googlecast._tcp.local.";

/// Bit 0 of the Cast `ca` TXT capability mask (Chromium
/// `CastDeviceCapability::VIDEO_OUT`). Devices without it — Chromecast Audio,
/// Google/Nest speakers, cast groups — can only receive audio.
const CA_VIDEO_OUT: u32 = 1;

/// Parses the `ca` (capabilities) TXT value. Missing or unparseable values
/// default to video-capable so unknown devices are never hidden or blocked.
fn parse_capabilities(ca: Option<&str>) -> u32 {
    ca.and_then(|s| s.parse().ok()).unwrap_or(CA_VIDEO_OUT)
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

    while let Ok(event) = receiver.recv() {
        match event {
            ServiceEvent::ServiceResolved(info) => {
                let Some(addr) = info
                    .get_addresses()
                    .iter()
                    .find(|a| a.is_ipv4())
                    .or_else(|| info.get_addresses().iter().next())
                    .cloned()
                    .and_then(|v| match v {
                        ScopedIp::V4(ip) => Some(IpAddr::from(*ip.addr())),
                        ScopedIp::V6(ip) => Some(IpAddr::from(*ip.addr())),
                        _ => todo!(),
                    })
                else {
                    warn!("resolved {} without addresses", info.get_fullname());
                    continue;
                };

                let fullname = info.get_fullname();
                let name = info.get_property_val_str("fn").unwrap_or_else(|| {
                    fullname
                        .split("._googlecast")
                        .next()
                        .unwrap_or("Chromecast")
                });
                let port = info.get_port();
                let ca = parse_capabilities(info.get_property_val_str("ca"));

                // Chromecasts re-announce periodically; compare
                // against the known entry by reference and only
                // build (and log) a Device when something changed.
                let changed = {
                    let mut devices = state.devices.lock();
                    match devices.get(fullname) {
                        Some(e)
                            if e.name == name && e.addr == addr && e.port == port && e.ca == ca =>
                        {
                            false
                        }
                        _ => {
                            devices.insert(
                                fullname.to_string(),
                                Device {
                                    id: fullname.to_string(),
                                    name: name.to_string(),
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
                    let _ = state.events.send(Event::DevicesChanged);
                } else {
                    debug!("refreshed {name} at {addr}:{port}");
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                info!("lost {fullname}");
                if state.devices.lock().remove(&fullname).is_some() {
                    let _ = state.events.send(Event::DevicesChanged);
                }
            }
            other => debug!("mdns event: {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
