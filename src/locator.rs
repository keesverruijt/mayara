//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use log::{debug, error, info, trace, warn};
use miette::Result;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use serde::Serialize;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::{RadarError, Radars};
use crate::{navico, util};

#[derive(PartialEq, Eq, Copy, Clone, Serialize, Debug)]
pub enum LocatorId {
    GenBR24,
    Gen3Plus,
}

impl LocatorId {
    pub(crate) fn as_str(&self) -> &'static str {
        use LocatorId::*;
        match *self {
            GenBR24 => "Navico BR24",
            Gen3Plus => "Navico 3G/4G/HALO",
        }
    }
}

#[derive(Clone)]
pub struct RadarListenAddress {
    pub id: LocatorId,
    pub address: SocketAddr,
    pub brand: String,
    pub adress_request_packet: Option<&'static [u8]>, // Optional message to send to ask radar for address
    pub process: &'static dyn Fn(
        &[u8],       // message
        &SocketAddr, // from
        &Ipv4Addr,   // nic_addr
        &Arc<RwLock<Radars>>,
        &SubsystemHandle,
    ) -> Result<(), io::Error>,
}

// The only part of RadioListenAddress that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for RadarListenAddress {}

impl RadarListenAddress {
    pub fn new(
        id: LocatorId,
        address: &SocketAddr,
        brand: &str,
        ping: Option<&'static [u8]>,
        process: &'static dyn Fn(
            &[u8],
            &SocketAddr,
            &Ipv4Addr,
            &Arc<RwLock<Radars>>,
            &SubsystemHandle,
        ) -> Result<(), io::Error>,
    ) -> RadarListenAddress {
        RadarListenAddress {
            id,
            address: address.clone(),
            brand: brand.into(),
            adress_request_packet: ping,
            process,
        }
    }
}

struct LocatorInfo {
    sock: UdpSocket,
    nic_addr: Ipv4Addr,
    process: &'static dyn Fn(
        &[u8],       // message
        &SocketAddr, // from
        &Ipv4Addr,   // nic_addr
        &Arc<RwLock<Radars>>,
        &SubsystemHandle,
    ) -> Result<(), io::Error>,
}

// The only part of LocatorInfo that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for LocatorInfo {}

#[async_trait]
pub trait RadarLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>);
}

struct InterfaceState {
    interface: Option<String>, // If set, listen only here (even if lo0 or lo)
    active_nic_addresses: Vec<Ipv4Addr>,
    inactive_nic_names: HashMap<String, u32>,
    lost_nic_names: HashMap<String, u32>,
    first_loop: bool,
}

pub struct Locator {
    pub radars: Arc<RwLock<Radars>>,
    pub interface: Option<String>,
}

impl Locator {
    pub fn new(radars: Arc<RwLock<Radars>>, interface: Option<String>) -> Self {
        Locator { radars, interface }
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        let radars = self.radars;
        let navico_locator = navico::create_locator();
        let navico_br24_locator = navico::create_br24_locator();
        //let mut garmin_locator = garmin::create_locator();

        let mut listen_addresses: Vec<RadarListenAddress> = Vec::new();
        navico_locator.update_listen_addresses(&mut listen_addresses);
        navico_br24_locator.update_listen_addresses(&mut listen_addresses);
        // garmin_locator.update_listen_addresses(&listen_addresses);

        info!("Entering loop, listening for radars");
        let mut interface_state = InterfaceState {
            interface: self.interface,
            active_nic_addresses: Vec::new(),
            inactive_nic_names: HashMap::new(),
            lost_nic_names: HashMap::new(),
            first_loop: true,
        };

        loop {
            let cancellation_token = subsys.create_cancellation_token();

            // create a list of sockets for all listen addresses
            let sockets = create_multicast_sockets(&listen_addresses, &mut interface_state);
            let mut set = JoinSet::new();
            if sockets.is_err() {
                if interface_state.interface.is_some() {
                    return Err(sockets.err().unwrap());
                }
                debug!("No NIC addresses found");
                sleep(Duration::from_millis(5000)).await;
            }
            let sockets = sockets.unwrap();

            for socket in sockets {
                spawn_receive(&mut set, socket);
            }
            set.spawn(async move {
                cancellation_token.cancelled().await;
                Err(RadarError::Shutdown)
            });
            set.spawn(async move {
                sleep(Duration::from_millis(30000)).await;
                Err(RadarError::Timeout)
            });

            // Now that we're listening to the radars, send any address request (wake) packets
            {
                for x in &listen_addresses {
                    if let Some(address_request) = x.adress_request_packet {
                        send_multicast_packet(&x.address, address_request);
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
                                    &radars,
                                    &subsys,
                                );
                                // Respawn this task
                                spawn_receive(&mut set, socket);
                            }
                            Err(e) => {
                                match e {
                                    RadarError::Shutdown => {
                                        info!("Locator shutdown");
                                        return Ok(());
                                    }
                                    RadarError::Timeout => {
                                        // Loop, reread everything
                                        break;
                                    }
                                    _ => {}
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
        }
    }
}

fn spawn_receive(
    set: &mut JoinSet<Result<(LocatorInfo, SocketAddr, Vec<u8>), RadarError>>,
    socket: LocatorInfo,
) {
    set.spawn(async move {
        let mut buf: Vec<u8> = Vec::with_capacity(2048);
        let res = socket.sock.recv_buf_from(&mut buf).await;

        match res {
            Ok((_, addr)) => Ok((socket, addr, buf)),
            Err(e) => Err(RadarError::Io(e)),
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
    interface_state: &mut InterfaceState,
) -> Result<Vec<LocatorInfo>, RadarError> {
    let only_interface = &interface_state.interface;

    match NetworkInterface::show() {
        Ok(interfaces) => {
            let mut sockets = Vec::new();
            for itf in interfaces {
                let mut active: bool = false;

                if only_interface.is_none() || only_interface.as_ref() == Some(&itf.name) {
                    for nic_addr in itf.addr {
                        if let IpAddr::V4(nic_ip) = nic_addr.ip() {
                            if !nic_ip.is_loopback() || only_interface.is_some() {
                                if interface_state.lost_nic_names.contains_key(&itf.name)
                                    || !interface_state.active_nic_addresses.contains(&nic_ip)
                                {
                                    if interface_state
                                        .inactive_nic_names
                                        .remove(&itf.name)
                                        .is_some()
                                    {
                                        info!(
                                        "Interface '{}' became active or gained an IPv4 address, now listening on IP address {}/{} for radars",
                                        itf.name,
                                        &nic_ip,
                                        nic_addr.netmask().unwrap()
                                    );
                                    } else {
                                        info!(
                                        "Interface '{}' now listening on IP address {}/{} for radars",
                                        itf.name,
                                        &nic_ip,
                                        nic_addr.netmask().unwrap()
                                    );
                                    }
                                    interface_state.active_nic_addresses.push(nic_ip.clone());
                                    interface_state.lost_nic_names.remove(&itf.name);
                                }

                                for radar_listen_address in listen_addresses {
                                    if let SocketAddr::V4(listen_addr) =
                                        radar_listen_address.address
                                    {
                                        let socket = util::create_multicast(&listen_addr, &nic_ip);
                                        match socket {
                                            Ok(socket) => {
                                                sockets.push(LocatorInfo {
                                                    sock: socket,
                                                    nic_addr: nic_ip.clone(),
                                                    process: radar_listen_address.process,
                                                });
                                                debug!(
                                                "Listening on '{}' address {} for multicast address {}",
                                                itf.name, nic_ip, listen_addr,
                                            );
                                            }
                                            Err(e) => {
                                                warn!(
                                                "Cannot listen on '{}' address {} for multicast address {}: {}",
                                                itf.name, nic_ip, listen_addr, e
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
                                active = true;
                            }
                        }
                    }
                    if interface_state.interface.is_some()
                        && interface_state.active_nic_addresses.len() == 0
                    {
                        return Err(RadarError::InterfaceNoV4(
                            interface_state.interface.clone().unwrap(),
                        ));
                    }
                }
                if !active && only_interface.is_none() {
                    if interface_state
                        .inactive_nic_names
                        .insert(itf.name.to_owned(), 0)
                        .is_none()
                    {
                        if interface_state.first_loop {
                            warn!("Interface '{}' does not have an IPv4 address", itf.name);
                        } else {
                            warn!(
                                "Interface '{}' became inactive or lost its IPv4 address",
                                itf.name
                            );
                            interface_state.lost_nic_names.insert(itf.name, 0);
                        }
                    }
                }
            }
            interface_state.first_loop = false;

            if interface_state.interface.is_some()
                && interface_state.active_nic_addresses.len() == 0
            {
                return Err(RadarError::InterfaceNotFound(
                    interface_state.interface.clone().unwrap(),
                ));
            }
            Ok(sockets)
        }
        Err(_) => Err(RadarError::EnumerationFailed),
    }
}
