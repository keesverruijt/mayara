use async_trait::async_trait;
use log::{debug, info};
use std::fmt;
use std::fmt::Display;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::sleep;

use super::{join_multicast, RadarLocator};
mod report;

const GARMIN_REPORT_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 254, 2, 0)), 50100);
const GARMIN_SEND_PORT: u16 = 50101;
const GARMIN_DATA_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(239, 254, 2, 0)), 50102);

pub struct RadarLocationInfo {
    addr: SocketAddr,              // The assigned IP address of the radar
    spoke_data_addr: SocketAddr,   // Where the radar will send data spokes
    report_addr: SocketAddr,       // Where the radar will send reports
    send_command_addr: SocketAddr, // Where displays will send commands to the radar
}

impl RadarLocationInfo {
    fn new(addr: SocketAddr, data: SocketAddr, report: SocketAddr, send: SocketAddr) -> Self {
        RadarLocationInfo {
            addr: addr,
            spoke_data_addr: data,
            report_addr: report,
            send_command_addr: send,
        }
    }
}

impl Display for RadarLocationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Radar ")?;
        write!(f, "[Garmin] at {}", &self.addr)?;
        write!(
            f,
            " multicast addr {}/{}/{}",
            &self.spoke_data_addr, &self.report_addr, &self.send_command_addr
        )
    }
}

fn process_beacon(report: &[u8]) {
    if report.len() < 2 {
        return;
    }

    report::process(report);
}

struct GarminLocator {
    found: bool,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
}

impl GarminLocator {
    async fn start(&mut self) -> io::Result<()> {
        match join_multicast(GARMIN_REPORT_ADDRESS).await {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "Listening on {} for Garmin xHD (and others?)",
                    GARMIN_REPORT_ADDRESS
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
                            if self.found {
                                process_beacon(&self.buf);
                            } else {
                                let mut radar_send = from.clone();

                                radar_send.set_port(GARMIN_SEND_PORT);
                                let location_info: RadarLocationInfo = RadarLocationInfo::new(
                                    from,
                                    GARMIN_DATA_ADDRESS,
                                    GARMIN_REPORT_ADDRESS,
                                    radar_send,
                                );
                                info!("Located {}", &location_info);
                                self.found = true;
                            }
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
        found: false,
        buf: Vec::with_capacity(2048),
        sock: None,
    };
    Box::new(locator)
}
