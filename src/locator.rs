//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use log::{debug, error, info, trace, warn};
use miette::Result;
use network_interface::{NetworkInterface, NetworkInterfaceConfig};
use serde::Serialize;
use tokio::sync::{broadcast, mpsc};
use tokio::{net::UdpSocket, sync::mpsc::Sender, task::JoinSet, time::sleep};
use tokio_graceful_shutdown::SubsystemHandle;

#[cfg(feature = "furuno")]
use crate::brand::furuno;
#[cfg(feature = "navico")]
use crate::brand::navico;
#[cfg(feature = "raymarine")]
use crate::brand::raymarine;

use crate::radar::{RadarError, SharedRadars};
use crate::{network, Brand, InterfaceApi, InterfaceId, RadarInterfaceApi, GLOBAL_ARGS};

const LOCATOR_PACKET_BUFFER_LEN: usize = 300; // Long enough for any location packet

#[derive(PartialEq, Eq, Copy, Clone, Serialize, Debug)]
pub enum LocatorId {
    GenBR24,
    Gen3Plus,
    Furuno,
    Raymarine,
}

impl LocatorId {
    pub(crate) fn as_str(&self) -> &'static str {
        use LocatorId::*;
        match *self {
            GenBR24 => "Navico BR24",
            Gen3Plus => "Navico 3G/4G/HALO",
            Furuno => "Furuno DRSxxxx",
            Raymarine => "Raymarine",
        }
    }
}

pub struct LocatorAddress {
    pub id: LocatorId,
    pub address: SocketAddr,
    pub brand: Brand,
    pub adress_request_packet: Option<&'static [u8]>, // Optional message to send to ask radar for address
    pub locator: Box<dyn RadarLocatorState>,
}

// The only part of RadioListenAddress that isn't Send is process, but since this is static it really
// is safe to send.
unsafe impl Send for LocatorAddress {}

impl LocatorAddress {
    pub fn new(
        id: LocatorId,
        address: &SocketAddr,
        brand: Brand,
        adress_request_packet: Option<&'static [u8]>,
        locator: Box<dyn RadarLocatorState>,
    ) -> LocatorAddress {
        LocatorAddress {
            id,
            address: address.clone(),
            brand,
            adress_request_packet,
            locator,
        }
    }
}

struct LocatorSocket {
    sock: UdpSocket,
    nic_addr: Ipv4Addr,
    state: Box<dyn RadarLocatorState>,
}

pub trait RadarLocatorState: Send {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error>;

    fn clone(&self) -> Box<dyn RadarLocatorState>;
}

pub trait RadarLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>);
}

struct InterfaceState {
    active_nic_addresses: Vec<Ipv4Addr>,
    inactive_nic_names: HashSet<String>,
    lost_nic_names: HashSet<String>,
    interface_api: InterfaceApi,
    first_loop: bool,
}

enum ResultType {
    Locator(LocatorSocket, SocketAddrV4, Vec<u8>),
    InterfaceRequest(mpsc::Sender<InterfaceApi>),
}

pub struct Locator {
    pub radars: SharedRadars,
}

impl Locator {
    pub fn new(radars: SharedRadars) -> Self {
        Locator { radars }
    }

    pub async fn run(
        self,
        subsys: SubsystemHandle,
        tx_ip_change: Sender<()>,
        tx_interface_request: broadcast::Sender<Option<Sender<InterfaceApi>>>,
    ) -> Result<(), RadarError> {
        let radars = &self.radars;

        debug!("Entering loop, listening for radars");
        let mut interface_state = InterfaceState {
            active_nic_addresses: Vec::new(),
            inactive_nic_names: HashSet::new(),
            lost_nic_names: HashSet::new(),
            interface_api: InterfaceApi {
                brands: HashSet::new(),
                interfaces: HashMap::new(),
            },
            first_loop: true,
        };

        let listen_addresses = Self::compute_listen_addresses(&mut interface_state);

        loop {
            let cancellation_token = subsys.create_cancellation_token();
            let child_token = cancellation_token.child_token();

            // create a list of sockets for all listen addresses
            let sockets = self.create_listen_sockets(&listen_addresses, &mut interface_state);
            let mut set = JoinSet::new();
            if sockets.is_err() {
                if GLOBAL_ARGS.interface.is_some() {
                    return Err(sockets.err().unwrap());
                }
                debug!("No NIC addresses found");
                sleep(Duration::from_millis(5000)).await;
            }
            let sockets: Vec<LocatorSocket> = sockets.unwrap();

            for socket in sockets {
                spawn_receive(&mut set, socket);
            }
            set.spawn(async move {
                cancellation_token.cancelled().await;
                Err(RadarError::Shutdown)
            });
            let tx_ip_change = tx_ip_change.clone();
            set.spawn(async move {
                if let Err(e) = network::wait_for_ip_addr_change(child_token).await {
                    match e {
                        RadarError::Shutdown => {
                            return Err(RadarError::Shutdown);
                        }
                        _ => {
                            log::error!("Failed to wait for IP change: {e}");
                            sleep(Duration::from_secs(30)).await;
                        }
                    }
                }
                let _ = tx_ip_change.send(()).await;

                Err(RadarError::Timeout)
            });
            spawn_interface_request(&mut set, &tx_interface_request);

            // Now that we're listening to the radars, send any address request (wake) packets
            if !GLOBAL_ARGS.replay {
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
                            Ok(ResultType::Locator(mut locator_socket, addr, buf)) => {
                                trace!(
                                    "{} via {} -> {:02X?}",
                                    &addr,
                                    &locator_socket.nic_addr,
                                    &buf
                                );

                                let _ = locator_socket.state.process(
                                    &buf,
                                    &addr,
                                    &locator_socket.nic_addr,
                                    &radars,
                                    &subsys,
                                );
                                // Respawn this task
                                spawn_receive(&mut set, locator_socket);
                            }
                            Ok(ResultType::InterfaceRequest(reply_channel)) => {
                                // Respawn this task
                                self.reply_with_interface_state(&interface_state, reply_channel)
                                    .await;

                                // Respawn this task
                                spawn_interface_request(&mut set, &tx_interface_request);
                            }
                            Err(e) => {
                                match e {
                                    RadarError::Shutdown => {
                                        debug!("Locator shutdown");
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

    async fn reply_with_interface_state(
        &self,
        interface_state: &InterfaceState,
        reply_channel: Sender<InterfaceApi>,
    ) {
        let mut interface_api = interface_state.interface_api.clone();

        for (_, radar_interface_api) in interface_api.interfaces.iter_mut() {
            if let (Some(listeners), Some(ip)) =
                (&mut radar_interface_api.listeners, &radar_interface_api.ip)
            {
                for (brand, status) in listeners.iter_mut() {
                    if self.radars.is_active_radar(brand, ip) {
                        *status = "Active".to_owned();
                    }
                }
            }
        }
        let _ = reply_channel.send(interface_api).await;
    }

    fn compute_listen_addresses(interface_state: &mut InterfaceState) -> Vec<LocatorAddress> {
        let mut listen_addresses: Vec<LocatorAddress> = Vec::new();
        let mut locators: Vec<Box<dyn RadarLocator>> = Vec::new();

        let brands = &mut interface_state.interface_api.brands;
        brands.clear();

        #[cfg(feature = "navico")]
        if GLOBAL_ARGS.brand.unwrap_or(Brand::Navico) == Brand::Navico {
            locators.push(navico::create_locator());
            locators.push(navico::create_br24_locator());
            brands.insert(Brand::Navico);
        }
        #[cfg(feature = "furuno")]
        if GLOBAL_ARGS.brand.unwrap_or(Brand::Furuno) == Brand::Furuno {
            locators.push(furuno::create_locator());
            brands.insert(Brand::Furuno);
        }
        #[cfg(feature = "raymarine")]
        if GLOBAL_ARGS.brand.unwrap_or(Brand::Raymarine) == Brand::Raymarine {
            locators.push(raymarine::create_locator());
            brands.insert(Brand::Raymarine);
        }

        locators
            .iter()
            .for_each(|x| x.set_listen_addresses(&mut listen_addresses));

        listen_addresses
    }

    fn create_listen_sockets(
        &self,
        listen_addresses: &Vec<LocatorAddress>,
        interface_state: &mut InterfaceState,
    ) -> Result<Vec<LocatorSocket>, RadarError> {
        let only_interface = &GLOBAL_ARGS.interface;
        let avoid_wifi = !GLOBAL_ARGS.allow_wifi;

        let if_api = &mut interface_state.interface_api.interfaces;
        if_api.clear();

        match NetworkInterface::show() {
            Ok(interfaces) => {
                trace!("getifaddrs() dump {:#?}", interfaces);
                let mut sockets = Vec::new();
                for itf in interfaces {
                    let mut active: bool = false;

                    if only_interface.is_none() || only_interface.as_ref() == Some(&itf.name) {
                        for nic_addr in itf.addr {
                            if let (IpAddr::V4(nic_ip), Some(IpAddr::V4(nic_netmask))) =
                                (nic_addr.ip(), nic_addr.netmask())
                            {
                                if avoid_wifi && network::is_wireless_interface(&itf.name) {
                                    trace!("Ignoring wireless interface '{}'", itf.name);
                                    if_api.insert(
                                        InterfaceId::new(&itf.name, Some(&nic_addr.ip())),
                                        RadarInterfaceApi::new(
                                            Some("Wireless ignored".to_owned()),
                                            None,
                                            None,
                                        ),
                                    );
                                    continue;
                                }
                                let mut listeners = HashMap::new();

                                if !nic_ip.is_loopback() || only_interface.is_some() {
                                    if interface_state.lost_nic_names.contains(&itf.name)
                                        || !interface_state.active_nic_addresses.contains(&nic_ip)
                                    {
                                        if interface_state.inactive_nic_names.remove(&itf.name) {
                                            info!(
                                            "Searching for radars on interface '{}' address {} (added/modified)",
                                            itf.name,
                                            &nic_ip,
                                        );
                                        } else {
                                            info!(
                                                "Searching for radars on interface '{}' address {}",
                                                itf.name, &nic_ip,
                                            );
                                        }
                                        interface_state.active_nic_addresses.push(nic_ip.clone());
                                        interface_state.lost_nic_names.remove(&itf.name);
                                    }

                                    for radar_listen_address in listen_addresses {
                                        if let SocketAddr::V4(listen_addr) =
                                            radar_listen_address.address
                                        {
                                            let socket = if !listen_addr.ip().is_multicast()
                                                && !network::match_ipv4(
                                                    &nic_ip,
                                                    listen_addr.ip(),
                                                    &nic_netmask,
                                                ) {
                                                if only_interface.is_none() {
                                                    Err(std::io::Error::new(
                                                        std::io::ErrorKind::AddrNotAvailable,
                                                        format!(
                                                            "No match for {}",
                                                            listen_addr.ip()
                                                        ),
                                                    ))
                                                } else {
                                                    network::create_udp_listen(
                                                        &listen_addr,
                                                        &nic_ip,
                                                        true,
                                                    )
                                                }
                                            } else {
                                                network::create_udp_listen(
                                                    &listen_addr,
                                                    &nic_ip,
                                                    false,
                                                )
                                            };

                                            let status = match socket {
                                                Ok(socket) => {
                                                    sockets.push(LocatorSocket {
                                                        sock: socket,
                                                        nic_addr: nic_ip.clone(),
                                                        state: radar_listen_address.locator.clone(),
                                                    });
                                                    debug!(
                                                        "Listening on '{}' address {} for address {}",
                                                        itf.name, nic_ip, listen_addr,
                                                    );
                                                    "Listening".to_owned()
                                                }
                                                Err(e) => {
                                                    warn!(
                                                    "Cannot listen on '{}' address {} for address {}: {}",
                                                    itf.name, nic_ip, listen_addr, e
                                                );
                                                    e.to_string()
                                                }
                                            };
                                            listeners
                                                .insert(radar_listen_address.brand.clone(), status);
                                        } else {
                                            trace!(
                                                "Ignoring IPv6 address {:?}",
                                                &radar_listen_address.address
                                            );
                                        }
                                    }
                                    active = true;
                                }

                                if_api.insert(
                                    InterfaceId::new(&itf.name, Some(&nic_addr.ip())),
                                    RadarInterfaceApi::new(None, Some(nic_ip), Some(listeners)),
                                );
                            }
                        }
                        if GLOBAL_ARGS.interface.is_some()
                            && interface_state.active_nic_addresses.len() == 0
                        {
                            return Err(RadarError::InterfaceNoV4(
                                GLOBAL_ARGS.interface.clone().unwrap(),
                            ));
                        }
                    }
                    if !active && only_interface.is_none() {
                        if interface_state
                            .inactive_nic_names
                            .insert(itf.name.to_owned())
                        {
                            if interface_state.first_loop {
                                trace!("Interface '{}' does not have an IPv4 address", itf.name);
                            } else {
                                warn!(
                                    "Interface '{}' became inactive or lost its IPv4 address",
                                    itf.name
                                );
                                interface_state.lost_nic_names.insert(itf.name.to_owned());
                            }
                        }
                        if_api.insert(
                            InterfaceId::new(&itf.name, None),
                            RadarInterfaceApi::new(Some("No IPv4 address".to_owned()), None, None),
                        );
                    }
                }
                interface_state.first_loop = false;

                if GLOBAL_ARGS.interface.is_some()
                    && interface_state.active_nic_addresses.len() == 0
                {
                    return Err(RadarError::InterfaceNotFound(
                        GLOBAL_ARGS.interface.clone().unwrap(),
                    ));
                }

                log::trace!("lost_nic_names = {:?}", interface_state.lost_nic_names);
                log::trace!(
                    "active_nic_addresses = {:?}",
                    interface_state.active_nic_addresses
                );
                Ok(sockets)
            }
            Err(_) => Err(RadarError::EnumerationFailed),
        }
    }
}

fn spawn_interface_request(
    set: &mut JoinSet<std::result::Result<ResultType, RadarError>>,
    tx_interface_request: &broadcast::Sender<Option<Sender<InterfaceApi>>>,
) {
    let mut rx_interface_request = tx_interface_request.subscribe();
    set.spawn(async move {
        match rx_interface_request.recv().await {
            Ok(Some(reply_channel)) => Ok(ResultType::InterfaceRequest(reply_channel)),
            _ => Err(RadarError::Shutdown),
        }
    });
}

fn spawn_receive(set: &mut JoinSet<Result<ResultType, RadarError>>, socket: LocatorSocket) {
    set.spawn(async move {
        let mut buf: Vec<u8> = Vec::with_capacity(LOCATOR_PACKET_BUFFER_LEN);
        let res = socket.sock.recv_buf_from(&mut buf).await;

        match res {
            Ok((_, addr)) => match addr {
                SocketAddr::V4(addr) => Ok(ResultType::Locator(socket, addr, buf)),
                SocketAddr::V6(addr) => Err(RadarError::InterfaceNoV4(format!("{}", addr))),
            },
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
