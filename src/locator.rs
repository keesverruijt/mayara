//
// The locator finds all radars by listening for known packets on the network.
// 
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present. 
// Still, we use this location method for all radars so the process is uniform.
//

const NAVICO_BEACON_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(236, 6, 7, 5);
const NAVICO_BEACON_PORT: u16 = 6878; 
const NAVICO_BEACON_ADDRESS: SocketAddr = SocketAddr::new(IpAddr::V4(NAVICO_BEACON_IPV4_ADDRESS), NAVICO_BEACON_PORT); 
   
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use tokio::net::UdpSocket;
use tokio_shutdown;
use tokio_shutdown::Shutdown;
use socket2::{Domain, Type, Protocol};

use log::info;

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

async fn join_multicast(addr: SocketAddr) -> io::Result<UdpSocket> {
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

    bind_multicast(&socket, addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub async fn new(shutdown: Shutdown) -> io::Result<()> {

    let shutdown_handle = shutdown.handle();
    let sock: UdpSocket = join_multicast(NAVICO_BEACON_ADDRESS).await?;

    let mut buf = Vec::with_capacity(2048);

    info!("Entering loop, listening for {}", NAVICO_BEACON_ADDRESS);

    // #[allow(dead_code)]
    tokio::select! {
        _ = async {
            loop {
                let (len, from) = sock.recv_buf_from(&mut buf).await?;

                info!("Received {} bytes from {}", len, from);
                if len > 3000 {
                    break;
                }
            }

            // Help the rust type inferencer out
            Ok::<_, io::Error>(())
        } => {}
        _ = shutdown_handle => {
            info!("terminating locator loop");
        }
    }

    Ok(())

}