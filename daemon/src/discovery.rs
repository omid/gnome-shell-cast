use std::net::IpAddr;
use std::sync::Arc;
use std::thread;

use anyhow::Result;
use log::{debug, info, warn};
use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::{Event, SharedState};

const SERVICE_TYPE: &str = "_googlecast._tcp.local.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// mDNS fullname; opaque, stable identifier used over D-Bus.
    pub id: String,
    /// Friendly name from the TXT record ("fn"), e.g. "Living Room TV".
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
}

/// Browses for Chromecast devices for the lifetime of the returned daemon,
/// keeping `state.devices` up to date and emitting DevicesChanged events.
pub fn start(state: Arc<SharedState>) -> Result<ServiceDaemon> {
    let mdns = ServiceDaemon::new()?;
    let receiver = mdns.browse(SERVICE_TYPE)?;

    thread::Builder::new()
        .name("mdns-discovery".into())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                match event {
                    ServiceEvent::ServiceResolved(info) => {
                        let Some(addr) = info
                            .get_addresses()
                            .iter()
                            .find(|a| a.is_ipv4())
                            .or_else(|| info.get_addresses().iter().next())
                            .copied()
                        else {
                            warn!("resolved {} without addresses", info.get_fullname());
                            continue;
                        };

                        let name = info
                            .get_property_val_str("fn")
                            .unwrap_or_else(|| {
                                info.get_fullname()
                                    .split("._googlecast")
                                    .next()
                                    .unwrap_or("Chromecast")
                            })
                            .to_string();

                        let device = Device {
                            id: info.get_fullname().to_string(),
                            name,
                            addr,
                            port: info.get_port(),
                        };
                        // Chromecasts re-announce periodically; only log and
                        // notify when the device is new or actually changed.
                        let changed = {
                            let mut devices = state.devices.lock().unwrap();
                            match devices.get(&device.id) {
                                Some(existing) if *existing == device => false,
                                _ => {
                                    devices.insert(device.id.clone(), device.clone());
                                    true
                                }
                            }
                        };
                        if changed {
                            info!("found {} at {}:{}", device.name, device.addr, device.port);
                            let _ = state.events.send(Event::DevicesChanged);
                        } else {
                            debug!(
                                "refreshed {} at {}:{}",
                                device.name, device.addr, device.port
                            );
                        }
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        info!("lost {fullname}");
                        if state.devices.lock().unwrap().remove(&fullname).is_some() {
                            let _ = state.events.send(Event::DevicesChanged);
                        }
                    }
                    other => debug!("mdns event: {other:?}"),
                }
            }
        })?;

    Ok(mdns)
}
