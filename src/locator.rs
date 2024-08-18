//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

use std::io::{self, ErrorKind};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use log::{debug, error, info, trace, warn};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_shutdown;
use tokio_shutdown::Shutdown;

use crate::radar::Radars;
use crate::{navico, util};

#[derive(Clone)]
pub struct RadarListenAddress {
    pub id: u32,
    pub address: SocketAddr,
    pub brand: String,
    pub ping: Option<&'static [u8]>, // Optional ping message to send to wake radar
    pub process: &'static dyn Fn(
        &[u8],       // message
        &SocketAddr, // from
        &Ipv4Addr,   // nic_addr
        &Arc<RwLock<Radars>>,
        &Shutdown,
    ) -> io::Result<()>,
}

// The only part of RadioListenAddress that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for RadarListenAddress {}

impl RadarListenAddress {
    pub fn new(
        id: u32,
        address: &SocketAddr,
        brand: &str,
        ping: Option<&'static [u8]>,
        process: &'static dyn Fn(
            &[u8],
            &SocketAddr,
            &Ipv4Addr,
            &Arc<RwLock<Radars>>,
            &Shutdown,
        ) -> io::Result<()>,
    ) -> RadarListenAddress {
        RadarListenAddress {
            id,
            address: address.clone(),
            brand: brand.into(),
            ping,
            process,
        }
    }
}

struct ListenSockets {
    sock: UdpSocket,
    nic_addr: Ipv4Addr,
    id: u32,
    process: &'static dyn Fn(
        &[u8],       // message
        &SocketAddr, // from
        &Ipv4Addr,   // nic_addr
        &Arc<RwLock<Radars>>,
        &Shutdown,
    ) -> io::Result<()>,
}

// The only part of ListenSockets that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for ListenSockets {}

#[async_trait]
pub trait RadarLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>);
}

pub async fn new(shutdown: Shutdown) -> io::Result<()> {
    let mut navico_locator = navico::create_locator();
    //let mut navico_br24_locator = navico::create_br24_locator();
    //let mut garmin_locator = garmin::create_locator();
    let detected_radars = Radars::new();

    let mut listen_addresses: Vec<RadarListenAddress> = Vec::new();
    navico_locator.update_listen_addresses(&mut listen_addresses);
    // navico_br24_locator.update_listen_addresses(&listen_addresses);
    // garmin_locator.update_listen_addresses(&listen_addresses);

    info!("Entering loop, listening for radars");

    loop {
        // create a list of sockets for all listen addresses
        let shutdown_handle = shutdown.clone().handle();
        let sockets = create_multicast_sockets(&listen_addresses);
        let mut set = JoinSet::new();
        if let Ok(sockets) = sockets {
            for socket in sockets {
                spawn_receive(&mut set, socket);
            }
            set.spawn(async move {
                shutdown_handle.await;
                Err(io::Error::new(ErrorKind::WriteZero, "shutdown"))
            });
            set.spawn(async move {
                sleep(Duration::from_millis(30000)).await;
                Err(io::Error::new(ErrorKind::Other, "timeout"))
            });

            // Now that we're listening to the radars, send any ping (wake) packets
            {
                for x in &listen_addresses {
                    if let Some(ping) = x.ping {
                        send_multicast_packet(&x.address, ping);
                    }
                }
            };

            while let Some(join_result) = set.join_next().await {
                match join_result {
                    Ok(join_result) => {
                        match join_result {
                            Ok((socket, addr, buf)) => {
                                trace!("{} via {} -> {:02X?}", &addr, &socket.nic_addr, &buf);

                                let _ = (socket.process)(
                                    &buf,
                                    &addr,
                                    &socket.nic_addr,
                                    &detected_radars,
                                    &shutdown,
                                );
                                // Respawn this task
                                spawn_receive(&mut set, socket);
                            }
                            Err(e) => {
                                if e.kind() == ErrorKind::WriteZero && e.to_string() == "shutdown" {
                                    // Shutdown!
                                    info!("Locator shutdown");
                                    return Ok(());
                                }
                                if e.kind() == ErrorKind::Other && e.to_string() == "timeout" {
                                    // Loop, reread everything
                                    break;
                                }
                                debug!("receive error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        debug!("JoinError: {}", e);
                    }
                };
            }
        } else {
            debug!("No NIC addresses found");
            sleep(Duration::from_millis(5000)).await;
        }

        /*
        loop {
            let detected_addr = &mut listen_addresses[0];
            let data: [u8; 3] = [0, 1, 2];
            (detected_addr.process)(
                &data,
                &detected_addr.address,
                0,
                detected_radars.clone(),
                shutdown.clone(),
            );
            break;
        }
        */
    }
}

fn spawn_receive(
    set: &mut JoinSet<Result<(ListenSockets, SocketAddr, Vec<u8>), io::Error>>,
    socket: ListenSockets,
) {
    set.spawn(async move {
        let mut buf: Vec<u8> = Vec::with_capacity(2048);
        let res = socket.sock.recv_buf_from(&mut buf).await;

        match res {
            Ok((_, addr)) => Ok((socket, addr, buf)),
            Err(e) => Err(e),
        }
    });
}

fn send_multicast_packet(addr: &SocketAddr, msg: &[u8]) {
    match NetworkInterface::show() {
        Ok(interfaces) => {
            for itf in interfaces {
                for nic_addr in itf.addr {
                    if let IpAddr::V4(nic_addr) = nic_addr.ip() {
                        if !nic_addr.is_loopback() {
                            // Send message via this IF
                            if let Ok(socket) = std::net::UdpSocket::bind(SocketAddr::new(
                                IpAddr::V4(nic_addr),
                                addr.port(),
                            )) {
                                if let Ok(_) = socket.send_to(msg, addr) {
                                    trace!("{} via {} <- {:02X?}", addr, nic_addr, msg);
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            error!("Unable to list Ethernet interfaces on this platform: {}", e);
        }
    }
}

fn create_multicast_sockets(
    listen_addresses: &Vec<RadarListenAddress>,
) -> io::Result<Vec<ListenSockets>> {
    match NetworkInterface::show() {
        Ok(interfaces) => {
            let mut sockets = Vec::new();
            for itf in interfaces {
                for nic_addr in itf.addr {
                    if let IpAddr::V4(nic_addr) = nic_addr.ip() {
                        if !nic_addr.is_loopback() {
                            for radar_listen_address in listen_addresses {
                                if let SocketAddr::V4(listen_addr) = radar_listen_address.address {
                                    let socket = util::create_multicast(&listen_addr, &nic_addr);
                                    match socket {
                                        Ok(socket) => {
                                            sockets.push(ListenSockets {
                                                sock: socket,
                                                nic_addr: nic_addr.clone(),
                                                id: radar_listen_address.id,
                                                process: radar_listen_address.process,
                                            });
                                            debug!(
                                                "Listening on {} address {} for multicast address {}",
                                                itf.name, nic_addr, listen_addr,
                                            );
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Cannot listen on {} address {} for multicast address {}: {}",
                                                itf.name, nic_addr, listen_addr, e
                                            );
                                        }
                                    }
                                } else {
                                    warn!(
                                        "Ignoring IPv6 address {:?}",
                                        &radar_listen_address.address
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Ok(sockets)
        }
        Err(e) => {
            error!("Unable to list Ethernet interfaces on this platform: {}", e);
            Err(io::Error::last_os_error())
        }
    }
}
