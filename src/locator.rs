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
use log::{debug, error, info, warn};
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_shutdown;
use tokio_shutdown::Shutdown;

use crate::radar::Radars;
use crate::{navico, util};

#[derive(Clone)]
pub struct RadioListenAddress {
    pub id: u32,
    pub address: SocketAddr,
    pub brand: String,
    pub process: &'static dyn Fn(
        &[u8],
        &SocketAddr,
        &Ipv4Addr,
        u32,
        Arc<RwLock<Radars>>,
        Shutdown,
    ) -> io::Result<()>,
}

// The only part of RadioListenAddress that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for RadioListenAddress {}

impl RadioListenAddress {
    pub fn new(
        id: u32,
        address: &SocketAddr,
        brand: &str,
        process: &'static dyn Fn(
            &[u8],
            &SocketAddr,
            &Ipv4Addr,
            u32,
            Arc<RwLock<Radars>>,
            Shutdown,
        ) -> io::Result<()>,
    ) -> RadioListenAddress {
        RadioListenAddress {
            id,
            address: address.clone(),
            brand: brand.into(),
            process,
        }
    }
}

struct ListenSockets {
    sock: UdpSocket,
    nic_addr: Ipv4Addr,
}
#[async_trait]
pub trait RadarLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadioListenAddress>);
}

pub async fn new(shutdown: Shutdown) -> io::Result<()> {
    let mut navico_locator = navico::create_locator();
    //let mut navico_br24_locator = navico::create_br24_locator();
    //let mut garmin_locator = garmin::create_locator();
    let detected_radars = Radars::new();

    let mut listen_addresses: Vec<RadioListenAddress> = Vec::new();
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
                set.spawn(async move {
                    let mut buf: Vec<u8> = Vec::with_capacity(2048);
                    let res = socket.sock.recv_buf_from(&mut buf).await;

                    match res {
                        Ok((_, addr)) => Ok((socket, addr, buf)),
                        Err(e) => Err(e),
                    }
                });
            }
            set.spawn(async move {
                shutdown_handle.await;
                Err(io::Error::new(ErrorKind::WriteZero, "shutdown"))
            });

            while let Some(join_result) = set.join_next().await {
                match join_result {
                    Ok(join_result) => match join_result {
                        Ok((socket, addr, buf)) => {
                            debug!(
                                "Received on {} from {}: {:?}",
                                &socket.nic_addr, &addr, &buf
                            );
                        }
                        Err(e) => {
                            if e.kind() == ErrorKind::WriteZero {
                                // Shutdown!
                                info!("Locator shutdown");
                                return Ok(());
                            }
                            debug!("receive error: {}", e);
                        }
                    },
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

    /*
    tokio::select! {
        _ = navico_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = navico_br24_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = garmin_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = shutdown_handle => {
            info!("terminating locator loop");
        }
    }
    */

    Ok(())
}

fn create_multicast_sockets(
    listen_addresses: &Vec<RadioListenAddress>,
) -> io::Result<Vec<ListenSockets>> {
    match NetworkInterface::show() {
        Ok(interfaces) => {
            let mut sockets = Vec::new();
            for itf in interfaces {
                for nic_addr in itf.addr {
                    if let IpAddr::V4(nic_addr) = nic_addr.ip() {
                        if !nic_addr.is_loopback() {
                            let socket: socket2::Socket = util::new_socket()?;

                            for listen_addr in listen_addresses {
                                if let SocketAddr::V4(listen_addr) = listen_addr.address {
                                    socket.join_multicast_v4(&listen_addr.ip(), &nic_addr)?;
                                } else {
                                    warn!("Ignoring IPv6 address {:?}", &listen_addr.address);
                                }
                            }

                            if let Ok(socket) = UdpSocket::from_std(socket.into()) {
                                sockets.push(ListenSockets {
                                    sock: socket,
                                    nic_addr: nic_addr.clone(),
                                });
                                debug!(
                                    "Listening on {} address {} for mcast ...",
                                    itf.name, nic_addr
                                );
                            } else {
                                error!("Unable to listen on {} to mcast ...", nic_addr);
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
