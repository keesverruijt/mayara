//
// The locator finds all radars by listening for known packets on the network.
//
// Some radars can only be found by this method because they use fluent multicast
// addresses, some are "easier" to locate by a fixed method, or just assuming they
// are present.
// Still, we use this location method for all radars so the process is uniform.
//

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use log::info;
use socket2::{Domain, Protocol, Type};
use tokio::net::UdpSocket;
use tokio_shutdown;
use tokio_shutdown::Shutdown;

use crate::radar::Radars;
use crate::{garmin, navico};

#[async_trait]
pub trait RadarLocator {
    async fn process_beacons(
        &mut self,
        detected_radars: Arc<RwLock<Radars>>,
        shutdown: Shutdown,
    ) -> io::Result<()>;
}

pub async fn new(shutdown: Shutdown) -> io::Result<()> {
    let shutdown_handle = shutdown.handle();
    let mut navico_locator = navico::create_locator();
    let mut navico_br24_locator = navico::create_br24_locator();
    let mut garmin_locator = garmin::create_locator();
    let detected_radars = Radars::new();

    info!("Entering loop, listening for Navico and Garmin radars");

    tokio::select! {
        _ = navico_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = navico_br24_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = garmin_locator.process_beacons(detected_radars.clone(), shutdown.clone()) => {}
        _ = shutdown_handle => {
            info!("terminating locator loop");
        }
    }

    Ok(())
}
