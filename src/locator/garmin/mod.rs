use async_trait::async_trait;
use bincode::deserialize;
use log::{debug, error, trace};
use serde::Deserialize;
use std::fmt;
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::sleep;

use super::{join_multicast, RadarLocator};
mod report;

const GARMIN_BEACON_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(239, 254, 2, 0);
const GARMIN_BEACON_PORT: u16 = 50100;
const GARMIN_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(GARMIN_BEACON_IPV4_ADDRESS), GARMIN_BEACON_PORT);

pub struct RadarLocationInfo {
    serial_nr: String,             // Serial # for this radar
    which: Option<String>,         // "A", "B" or None
    addr: SocketAddr,              // The assigned IP address of the radar
    spoke_data_addr: SocketAddr,   // Where the radar will send data spokes
    report_addr: SocketAddr,       // Where the radar will send reports
    send_command_addr: SocketAddr, // Where displays will send commands to the radar
}

impl RadarLocationInfo {
    fn new(
        serial_no: &str,
        which: Option<&str>,
        addr: SocketAddrV4,
        data: SocketAddrV4,
        report: SocketAddrV4,
        send: SocketAddrV4,
    ) -> Self {
        RadarLocationInfo {
            serial_nr: serial_no.to_owned(),
            which: which.map(String::from),
            addr: addr.into(),
            spoke_data_addr: data.into(),
            report_addr: report.into(),
            send_command_addr: send.into(),
        }
    }
}

impl Display for RadarLocationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Radar ")?;
        if let Some(which) = &self.which {
            write!(f, "{} ", which)?;
        }
        write!(f, "[{}] at {}", &self.serial_nr, &self.addr)?;
        write!(
            f,
            " multicast addr {}/{}/{}",
            &self.spoke_data_addr, &self.report_addr, &self.send_command_addr
        )
    }
}

fn process_beacon(report: &[u8], from: SocketAddr) {
    if report.len() < 2 {
        return;
    }

    trace!("{}: Garmin beacon: {:?}", from, report);
    report::process(report);
}

struct GarminLocator {
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
}

impl GarminLocator {
    async fn start(&mut self) -> io::Result<()> {
        match join_multicast(GARMIN_BEACON_ADDRESS).await {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "Listening on {} for Garmin xHD (and others?)",
                    GARMIN_BEACON_ADDRESS
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!("Beacon multicast failed: {}", e);
                Ok(())
            }
        }
    }
}

#[async_trait]
impl RadarLocator for GarminLocator {
    async fn process_beacons(&mut self) -> io::Result<()> {
        self.start().await.unwrap();
        loop {
            match &self.sock {
                Some(sock) => {
                    self.buf.clear();
                    match sock.recv_buf_from(&mut self.buf).await {
                        Ok((_len, from)) => {
                            process_beacon(&self.buf, from);
                        }
                        Err(e) => {
                            debug!("Beacon read failed: {}", e);
                            self.sock = None;
                        }
                    }
                }
                None => {
                    sleep(Duration::from_millis(1000)).await;
                    self.start().await.unwrap();
                }
            }
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = GarminLocator {
        buf: Vec::with_capacity(2048),
        sock: None,
    };
    Box::new(locator)
}
