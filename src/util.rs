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
use std::net::SocketAddrV4;
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
pub fn new_socket() -> io::Result<socket2::Socket> {
    let socket = socket2::Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;

    // we're going to use read timeouts so that we don't hang waiting for packets
    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;

    Ok(socket)
}

/// On Windows, unlike all Unix variants, it is improper to bind to the multicast address
///
/// see https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550(v=vs.85).aspx
#[cfg(windows)]
fn bind_to_multicast(socket: &socket2::Socket, addr: &SocketAddrV4) -> io::Result<()> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), addr.port());
    socket.bind(&socket2::SockAddr::from(addr))?;
    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_to_multicast(socket: &socket2::Socket, addr: &SocketAddrV4) -> io::Result<()> {
    let addr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    socket.bind(&socket2::SockAddr::from(addr))?;
    Ok(())
}

pub async fn create_multicast(addr: &SocketAddrV4, nic_addr: &Ipv4Addr) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    bind_to_multicast(&socket, addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}
