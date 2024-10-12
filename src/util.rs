// Various common functions

pub fn c_string(bytes: &[u8]) -> Option<&str> {
    let bytes_without_null = match bytes.iter().position(|&b| b == 0) {
        Some(ix) => &bytes[..ix],
        None => bytes,
    };

    std::str::from_utf8(bytes_without_null).ok()
}
pub fn c_wide_string(bytes: &[u8]) -> String {
    let mut res = String::new();

    let mut i = bytes.iter();
    while let (Some(lo), Some(hi)) = (i.next(), i.next()) {
        let c = *lo as u32 + ((*hi as u32) << 8);
        if c == 0 {
            break;
        }
        if let Some(c) = std::char::from_u32(c) {
            res.push(c);
        }
    }
    res
}

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
                        0 => ' ',
                        1..512 => '.',
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
fn bind_to_multicast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    let socketaddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr))?;
    log::info!("Binding multicast socket to {}", socketaddr);

    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_to_multicast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    // Linux is special, if we don't disable IP_MULTICAST_ALL the kernel forgets on
    // which device the multicast packet arrived and sends it to all sockets.
    #[cfg(target_os = "linux")]
    {
        use std::{io, mem, os::unix::io::AsRawFd};

        unsafe {
            let optval: libc::c_int = 0;
            let ret = libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_IP,
                libc::IP_MULTICAST_ALL,
                &optval as *const _ as *const libc::c_void,
                mem::size_of_val(&optval) as libc::socklen_t,
            );
            if ret != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }

    socket.join_multicast_v4(addr.ip(), nic_addr)?;

    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr))?;
    log::info!(
        "Binding multicast socket to {} nic {}",
        socketaddr,
        nic_addr
    );

    Ok(())
}

/// On Windows, unlike all Unix variants, it is improper to bind to the multicast address
///
/// see https://msdn.microsoft.com/en-us/library/windows/desktop/ms737550(v=vs.85).aspx
#[cfg(windows)]
fn bind_to_broadcast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    let _ = socket.set_broadcast(true);
    let _ = addr; // Not used on Windows

    let socketaddr = SocketAddr::new(IpAddr::V4(*nic_addr), addr.port());

    socket.bind(&socket2::SockAddr::from(socketaddr))?;
    log::info!("Binding broadcast socket to {}", socketaddr);
    Ok(())
}

/// On unixes we bind to the multicast address, which causes multicast packets to be filtered
#[cfg(unix)]
fn bind_to_broadcast(
    socket: &socket2::Socket,
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<()> {
    let _ = socket.set_broadcast(true);
    let _ = nic_addr; // Not used on Linux

    socket.bind(&socket2::SockAddr::from(*addr))?;
    log::info!("Binding broadcast socket to {}", *addr);
    Ok(())
}

pub fn create_udp_multicast_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    bind_to_multicast(&socket, addr, nic_addr)?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub fn create_udp_listen(
    addr: &SocketAddrV4,
    nic_addr: &Ipv4Addr,
    no_broadcast: bool,
) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    if addr.ip().is_multicast() {
        bind_to_multicast(&socket, addr, nic_addr)?;
    } else if !no_broadcast {
        bind_to_broadcast(&socket, addr, nic_addr)?;
    } else {
        let socketaddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), addr.port());

        socket.bind(&socket2::SockAddr::from(socketaddr))?;
        log::info!("Binding socket to {}", socketaddr);
    }

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub fn create_multicast_send(addr: &SocketAddrV4, nic_addr: &Ipv4Addr) -> io::Result<UdpSocket> {
    let socket: socket2::Socket = new_socket()?;

    let socketaddr = SocketAddr::new(IpAddr::V4(*addr.ip()), addr.port());
    let socketaddr_nic = SocketAddr::new(IpAddr::V4(*nic_addr), addr.port());
    socket.bind(&socket2::SockAddr::from(socketaddr_nic))?;
    socket.connect(&socket2::SockAddr::from(socketaddr))?;

    let socket = UdpSocket::from_std(socket.into())?;
    Ok(socket)
}

pub fn match_ipv4(addr: &Ipv4Addr, bcast: &Ipv4Addr, netmask: &Ipv4Addr) -> bool {
    let r = addr & netmask;
    let b = bcast & netmask;
    r == b
}
