// Various common functions

pub fn c_string(bytes: &[u8]) -> Option<&str> {
    let bytes_without_null = match bytes.iter().position(|&b| b == 0) {
        Some(ix) => &bytes[..ix],
        None => bytes,
    };

    std::str::from_utf8(bytes_without_null).ok()
}

use log::debug;
use network_interface::NetworkInterface;
use network_interface::NetworkInterfaceConfig;
use socket2::{Domain, Protocol, Type};
use std::{
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::net::UdpSocket;

pub struct PrintableSlice<'a>(&'a [u8]);

impl<'a> PrintableSlice<'a> {
    pub fn new<T>(data: &'a T) -> PrintableSlice<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        PrintableSlice(data.as_ref())
    }
}

// You can choose to implement multiple traits, like Lower and UpperPrintable

impl fmt::Display for PrintableSlice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sep: &str = "[";

        for byte in self.0 {
            if *byte >= 32 && *byte < 127 {
                write!(f, "{} {}", sep, *byte as char)?;
            } else {
                write!(f, "{} .", sep)?;
            }
            sep = "  ";
        }
        write!(f, "]")?;
        Ok(())
    }
}

pub struct PrintableSpoke<'a>(&'a [u8]);

impl<'a> PrintableSpoke<'a> {
    pub fn new<T>(data: &'a T) -> PrintableSpoke<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        PrintableSpoke(data.as_ref())
    }
}
impl fmt::Display for PrintableSpoke<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sum: u32 = 0;
        let mut count: u32 = 0;

        write!(f, "[")?;
        for byte in self.0 {
            sum += *byte as u32;
            count += 1;

            if count == 8 {
                write!(
                    f,
                    "{}",
                    match sum {
                        0..8 => ' ',
                        8..512 => '.',
                        _ => '*',
                    }
                )?;
                count = 0;
                sum = 0;
            }
        }
        if count > 4 {
            write!(
                f,
                "{}",
                match sum {
                    0..8 => ' ',
                    8..512 => '.',
                    _ => '*',
                }
            )?;
        }
        write!(f, "]")?;
        Ok(())
    }
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
pub fn bind_multicast(socket: &socket2::Socket, addr: SocketAddr) -> io::Result<()> {
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
fn bind_multicast(socket: &socket2::Socket, addr: &SocketAddr) -> io::Result<()> {
    socket.bind(&socket2::SockAddr::from(addr.clone()))?;
    Ok(())
}

pub async fn join_multicast(addr: &SocketAddr) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket(addr)?;

    let network_interfaces = NetworkInterface::show().unwrap();

    // Join all the network cards
    for itf in network_interfaces.iter() {
        for nic_addr in itf.addr.iter() {
            if let IpAddr::V4(nic_addr) = nic_addr.ip() {
                if !nic_addr.is_link_local() {
                    match addr.ip() {
                        IpAddr::V4(ref mdns_v4) => {
                            // join to the multicast address, with all interfaces
                            socket.join_multicast_v4(mdns_v4, &nic_addr)?;
                        }
                        IpAddr::V6(ref mdns_v6) => {
                            // join to the multicast address, with all interfaces (ipv6 uses indexes not addresses)
                            socket.join_multicast_v6(mdns_v6, 0)?;
                        }
                    };
                }
            }
        }
    }

    // let addr = SocketAddr::new(Ipv4Addr::new(10, 211, 55, 2).into(), 6878);

    bind_multicast(&socket, addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}
