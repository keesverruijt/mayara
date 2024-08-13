//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

mod garmin;
mod navico;

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use log::info;
use socket2::{Domain, Protocol, Type};
use tokio::net::UdpSocket;
use tokio_shutdown;
use tokio_shutdown::Shutdown;

use crate::radar::{RadarLocationInfo, Radars};

#[async_trait]
pub trait RadarLocator {
    async fn process_beacons(&mut self, detected_radars: Arc<RwLock<Radars>>) -> io::Result<()>;
}

// this will be common for all our sockets
fn new_socket(addr: &SocketAddr) -> io::Result<socket2::Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV4
    };

    let socket = socket2::Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    // we're going to use read timeouts so that we don't hang waiting for packets
    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;

    Ok(socket)
}

/// On Windows, unlike all Unix variants, it is improper to bind to the multicast address
///
/// see https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550(v=vs.85).aspx
#[cfg(windows)]
fn bind_multicast(socket: &socket2::Socket, addr: SocketAddr) -> io::Result<()> {
    use core::net::Ipv6Addr;

    let addr = match addr {
        SocketAddr::V4(addr) => SocketAddr::new(Ipv4Addr::new(0, 0, 0, 0).into(), addr.port()),
        SocketAddr::V6(addr) => {
            SocketAddr::new(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 0).into(), addr.port())
        }
    };

    socket.bind(&socket2::SockAddr::from(addr))?;
    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_multicast(socket: &socket2::Socket, addr: SocketAddr) -> io::Result<()> {
    socket.bind(&socket2::SockAddr::from(addr))?;
    Ok(())
}

pub async fn join_multicast(addr: SocketAddr) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket(&addr)?;

    // depending on the IP protocol we have slightly different work
    match addr.ip() {
        IpAddr::V4(ref mdns_v4) => {
            // join to the multicast address, with all interfaces
            socket.join_multicast_v4(mdns_v4, &Ipv4Addr::new(0, 0, 0, 0))?;
        }
        IpAddr::V6(ref mdns_v6) => {
            // join to the multicast address, with all interfaces (ipv6 uses indexes not addresses)
            socket.join_multicast_v6(mdns_v6, 0)?;
        }
    };

    // let addr = SocketAddr::new(Ipv4Addr::new(10, 211, 55, 2).into(), 6878);

    bind_multicast(&socket, addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub async fn new(shutdown: Shutdown) -> io::Result<()> {
    let shutdown_handle = shutdown.handle();
    let mut navico_locator = navico::create_locator();
    let mut navico_br24_locator = navico::create_br24_locator();
    let mut garmin_locator = garmin::create_locator();
    let detected_radars = Radars::new();

    info!("Entering loop, listening for Navico and Garmin radars");

    tokio::select! {
        _ = navico_locator.process_beacons(detected_radars.clone()) => {}
        _ = navico_br24_locator.process_beacons(detected_radars.clone()) => {}
        _ = garmin_locator.process_beacons(detected_radars.clone()) => {}
        _ = shutdown_handle => {
            info!("terminating locator loop");
        }
    }

    Ok(())
}
