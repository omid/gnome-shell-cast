//! Networking helpers shared by discovery, mirroring and the HTTP server.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};

/// A UDP socket bound to `target`'s address family but **not** connected, so
/// it still receives datagrams whose source does not exactly match `target` -
/// a Cast receiver answers RTP from a different port than it listens on.
pub fn bound_udp(target: IpAddr) -> io::Result<UdpSocket> {
    let bind = match target {
        IpAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
        IpAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    };
    UdpSocket::bind(bind)
}

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

    /// Connecting pins the local address, so RTCP sent to another of our
    /// addresses was dropped by the kernel; the RTP socket must stay wildcard.
    #[test]
    fn bound_socket_keeps_the_wildcard_local_address() {
        let v6 = bound_udp("::1".parse().unwrap()).unwrap();
        assert!(v6.local_addr().unwrap().ip().is_unspecified());
        let v4 = bound_udp("127.0.0.1".parse().unwrap()).unwrap();
        assert!(v4.local_addr().unwrap().ip().is_unspecified());
    }
}
