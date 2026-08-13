//! Networking helpers shared by discovery, mirroring and the HTTP server.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// A UDP socket connected to `target`, bound to a matching address family: an
/// IPv4 socket cannot connect to an IPv6 peer (`EAFNOSUPPORT`). Connecting a
/// UDP socket only consults the routing table, so this sends nothing and
/// doubles as a reachability probe.
pub fn connected_udp(target: IpAddr, port: u16) -> io::Result<UdpSocket> {
    let bind = match target {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    let socket = UdpSocket::bind(bind)?;
    socket.connect(SocketAddr::new(target, port))?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binding the wrong family used to drop mirroring to HLS whenever
    /// discovery landed on a device's AAAA record.
    #[test]
    fn socket_matches_the_target_address_family() {
        let v4 = connected_udp("127.0.0.1".parse().unwrap(), 9).unwrap();
        assert!(v4.peer_addr().unwrap().is_ipv4());
        let v6 = connected_udp("::1".parse().unwrap(), 9).unwrap();
        assert!(v6.peer_addr().unwrap().is_ipv6());
    }
}
