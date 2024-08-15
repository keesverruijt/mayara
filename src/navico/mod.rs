use async_trait::async_trait;
use bincode::deserialize;
use log::{debug, error, log_enabled, trace};
use serde::Deserialize;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::sleep;
use tokio_shutdown::Shutdown;

use crate::locator::RadarLocator;
use crate::radar::{located, RadarLocationInfo, Radars};
use crate::util::PrintableSlice;
use crate::util::{c_string, join_multicast};

mod receive;

const NAVICO_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 5)), 6878);
const NAVICO_COMMON_REPORT_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 9)), 6679);

#[derive(Deserialize, Debug, Copy, Clone)]
struct NetworkSocketAddrV4 {
    addr: Ipv4Addr,
    port: [u8; 2],
}

impl From<NetworkSocketAddrV4> for SocketAddrV4 {
    fn from(item: NetworkSocketAddrV4) -> Self {
        SocketAddrV4::new(item.addr, u16::from_be_bytes(item.port))
    }
}

/*
RADAR REPORTS

The radars send various reports. The first 2 bytes indicate what the report type is.
The types seen on a BR24 are:

2nd byte C4:   01 02 03 04 05 07 08
2nd byte F5:   08 0C 0D 0F 10 11 12 13 14

Not definitive list for
4G radars only send the C4 data.
*/

//
// The following is the received radar state. It sends this regularly
// but especially after something sends it a state change.
//

/*

 BR24:   N/A
 3G:     N/A
             Serial______________            Addr____Port                        Addr____Port        Addr____Port Addr____Port
Addr____Port                    Addr____Port        Addr____Port        Addr____Port                    Addr____Port Addr____Port
Addr____Port                    Addr____Port        Addr____Port        Addr____Port                    Addr____Port Addr____Port
Addr____Port 4G:
01B2313430333330323030300000000000000A0043D901010600FDFF20010200100000000A0043D9176011000000EC0607161A261F002001020010000000EC0607171A1C11000000EC0607181A1D10002001030010000000EC0607081A1611000000EC06070A1A1812000000EC0607091A1710002002030010000000EC06070D1A0111000000EC06070E1A0212000000EC06070F1A0312002001030010000000EC0607121A2011000000EC0607141A2212000000EC0607131A2112002002030010000000EC06070C1A0411000000EC06070D1A0512000000EC06070E1A06
 HALO:
01B231353039303332303030000000000000C0A800F014300600FDFF2001020010000000EC06076117F111000000EC0607161A261F002001020010000000EC06076217F211000000EC06076317F310002001030010000000EC06076417F411000000EC06076517F512000000EC06076617F610002002030010000000EC06076717F711000000EC06076817F812000000EC06076917F912002001030010000000EC06076A17FA11000000EC06076B17FB12000000EC06076C17FC12002002030010000000EC06076D17FD11000000EC06076E17FE12000000EC06076F17FF
 HALO24:
01B2313930323530313030300000000000000A0043C620310600FDFF2001020010000000EC060820197011000000EC0607161A261F002001020010000000EC060821197111000000EC060822197210002001030010000000EC060823197311000000EC060824197412000000EC060823197510002002030010000000EC060825197611000000EC060826197712000000EC060825197812002001030010000000EC060823197911000000EC060827197A12000000EC060823197B12002002030010000000EC060825197C11000000EC060828197D12000000EC060825197E

0A 00 43 D9 01 01 06 00 FD FF 20 01 02 00 10 00 00 00
0A 00 43 D9 17 60 11 00 00 00
EC 06 07 16 1A 26 1F 00 20 01 02 00 10 00 00 00
EC 06 07 17 1A 1C 11 00 00 00
EC 06 07 18 1A 1D 10 00 20 01 03 00 10 00 00 00
EC 06 07 08 1A 16 11 00 00 00
EC 06 07 0A 1A 18 12 00 00 00
EC 06 07 09 1A 17 10 00 20 02 03 00 10 00 00 00
EC 06 07 0D 1A 01 11 00 00 00
EC 06 07 0E 1A 02 12 00 00 00
EC 06 07 0F 1A 03 12 00 20 01 03 00 10 00 00 00
EC 06 07 12 1A 20 11 00 00 00
EC 06 07 14 1A 22 12 00 00 00
EC 06 07 13 1A 21 12 00 20 02 03 00 10 00 00 00
EC 06 07 0C 1A 04 11 00 00 00
EC 06 07 0D 1A 05 12 00 00 00
EC 06 07 0E 1A 06
*/
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconHeader {
    _id: u16,
    serial_no: [u8; 16],             // ASCII serial number, zero terminated
    radar_addr: NetworkSocketAddrV4, // 0A 00 43 D9 01 01
    _filler1: [u8; 12],              // 11000000
    _addr1: NetworkSocketAddrV4,     // EC0608201970
    _filler2: [u8; 4],               // 11000000
    _addr2: NetworkSocketAddrV4,     // EC0607161A26
    _filler3: [u8; 10],              // 1F002001020010000000
    _addr3: NetworkSocketAddrV4,     // EC0608211971
    _filler4: [u8; 4],               // 11000000
    _addr4: NetworkSocketAddrV4,     // EC0608221972
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconRadar {
    _filler1: [u8; 10],          // 10002001030010000000
    data: NetworkSocketAddrV4,   // EC0608231973
    _filler2: [u8; 4],           // 11000000
    send: NetworkSocketAddrV4,   // EC0608241974
    _filler3: [u8; 4],           // 12000000
    report: NetworkSocketAddrV4, // EC0608231975
}

// Radars that have one internal radar: 3G, Halo 20, etc.

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconSingle {
    header: NavicoBeaconHeader,
    a: NavicoBeaconRadar,
}

// As seen on all dual range radar (4G, HALO 20+, 24, 3, etc)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconDual {
    header: NavicoBeaconHeader,
    a: NavicoBeaconRadar,
    b: NavicoBeaconRadar,
    /* We don't care about the rest: */
    /*
    _filler11: [u8; 10],           // 12002001030010000000
    addr11: NetworkSocketAddrV4,        // EC0608231979
    _filler12: [u8; 4],            // 11000000
    addr12: NetworkSocketAddrV4,        // EC060827197A
    _filler13: [u8; 4],            // 12000000
    addr13: NetworkSocketAddrV4,        // EC060823197B
    _filler14: [u8; 10],           // 12002002030010000000
    addr14: NetworkSocketAddrV4,        // EC060825197C
    _filler15: [u8; 4],            // 11000000
    addr15: NetworkSocketAddrV4,        // EC060828197D
    _filler16: [u8; 4],            // 12000000
    addr16: NetworkSocketAddrV4,        // EC060825197E
    */
}

const NAVICO_BR24_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 4)), 6768);

/*  The following beacon package has been seen:
    1, 178, // 0: id =
    49, 48, 52, 55, 51, 48, 48, 48, 52, 51, 0, 0, 0, 0, 0, 0, // 2: Serial Nr
    169, 254, 210, 23, 1, 1 // 18: Radar IP address
    2, 0, 18, 0, 32, 1, 3, 0, 18, 0, 0, 0, //
    236, 6, 7, 19, 26, 33,
    17, 0, 0, 0,
    236, 6, 7, 20, 26, 34,
    16, 0, 0, 0,
    236, 6, 7, 18, 26, 32,
    16, 0, 32, 1, 3, 0, 18, 0, 0, 0,
    236, 6, 7, 9, 26, 23, // Report addr
    17, 0, 0, 0,
    236, 6, 7, 10, 26, 24,  // Send command addr
    16, 0, 0, 0,
    236, 6, 7, 8, 26, 22 // Data addr

*/

// The beacon message from BR24 (and first gen 3G) is _slightly_ different,
// so it needs a different structure. It is also sent to a different MultiCast address!
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct BR24Beacon {
    _id: u16,
    serial_no: [u8; 16], // ASCII serial number, zero terminated
    radar_addr: NetworkSocketAddrV4,
    _filler1: [u8; 12],
    _addr1: NetworkSocketAddrV4,
    _filler2: [u8; 4],
    _addr2: NetworkSocketAddrV4,
    _filler3: [u8; 4],
    _addr3: NetworkSocketAddrV4,
    _filler4: [u8; 10],
    data: NetworkSocketAddrV4, // Note different order from newer radars
    _filler5: [u8; 4],
    send: NetworkSocketAddrV4,
    _filler6: [u8; 4],
    report: NetworkSocketAddrV4,
}

// We _also_ listen on the 4G report address, as 4G older radars don't
// send the beacon message with the addresses often enough.

struct RadarReport03c4_129 {
    _what: u8,
    _command: u8,
    radar_type: u8,     // 00=Halo 01=4G + modern 3G 08=3G 0F=BR24
    _filler1: [u8; 31], // Lots of unknown stuff
    hours: u32,         // Hours of operation
    _filler2: [u8; 20], // More unknown
    firmware_date: [u8; 16],
    firmware_time: [u8; 16],
    _filler3: [u8; 7],
}

const NAVICO_BEACON_SINGLE_SIZE: usize = size_of::<NavicoBeaconSingle>();
const NAVICO_BEACON_DUAL_SIZE: usize = size_of::<NavicoBeaconDual>();
const NAVICO_BEACON_BR24_SIZE: usize = size_of::<BR24Beacon>();

async fn found(
    info: RadarLocationInfo,
    radars: &Arc<RwLock<Radars>>,
    shutdown: &Shutdown,
) -> io::Result<()> {
    if let Some(info) = located(info, radars) {
        // It's new, start the RadarProcessor thread
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut receiver = receive::Receive::new(info);
            receiver.run(shutdown).await.unwrap();
        });
        // TODO do something with the join handle
    }
    Ok(())
}

async fn process_report(
    report: &[u8],
    from: SocketAddr,
    radars: &Arc<RwLock<Radars>>,
    shutdown: &Shutdown,
) -> io::Result<()> {
    if report.len() < 2 {
        return Ok(());
    }

    if log_enabled!(log::Level::Trace) {
        trace!(
            "{}: Navico report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        trace!("{}: printable:     {}", from, PrintableSlice::new(report));
    }

    if report[0] == 0x01 && report[1] == 0xB1 {
        // Wake radar
        debug!("Wake radar request from {}", from);
        return Ok(());
    }
    if report[0] == 0x1 && report[1] == 0xB2 {
        // Common Navico message

        return process_beacon_report(report, from, radars, shutdown).await;
    }
    Ok(())
}

async fn process_beacon_report(
    report: &[u8],
    from: SocketAddr,
    radars: &Arc<RwLock<Radars>>,
    shutdown: &Shutdown,
) -> Result<(), io::Error> {
    if report.len() < size_of::<BR24Beacon>() {
        debug!("Incomplete beacon from {}, length {}", from, report.len());
        return Ok(());
    }

    if report.len() >= NAVICO_BEACON_DUAL_SIZE {
        match deserialize::<NavicoBeaconDual>(report) {
            Ok(data) => {
                if let Some(serial_no) = c_string(&data.header.serial_no) {
                    let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                    let radar_data: SocketAddrV4 = data.a.data.into();
                    let radar_report: SocketAddrV4 = data.a.report.into();
                    let radar_send: SocketAddrV4 = data.a.send.into();
                    let location_info: RadarLocationInfo = RadarLocationInfo::new(
                        "Navico",
                        None,
                        Some(serial_no),
                        Some("A"),
                        radar_addr.into(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, shutdown).await.unwrap();

                    let radar_data: SocketAddrV4 = data.b.data.into();
                    let radar_report: SocketAddrV4 = data.b.report.into();
                    let radar_send: SocketAddrV4 = data.b.send.into();
                    let location_info: RadarLocationInfo = RadarLocationInfo::new(
                        "Navico",
                        None,
                        Some(serial_no),
                        Some("B"),
                        radar_addr.into(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, shutdown).await.unwrap();
                }
            }
            Err(e) => {
                error!("Failed to decode dual range data: {}", e);
            }
        }
    } else if report.len() >= NAVICO_BEACON_SINGLE_SIZE {
        match deserialize::<NavicoBeaconSingle>(report) {
            Ok(data) => {
                if let Some(serial_no) = c_string(&data.header.serial_no) {
                    let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                    let radar_data: SocketAddrV4 = data.a.data.into();
                    let radar_report: SocketAddrV4 = data.a.report.into();
                    let radar_send: SocketAddrV4 = data.a.send.into();
                    let location_info: RadarLocationInfo = RadarLocationInfo::new(
                        "Navico",
                        None,
                        Some(serial_no),
                        None,
                        radar_addr.into(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, shutdown).await.unwrap();
                }
            }
            Err(e) => {
                error!("Failed to decode dual range data: {}", e);
            }
        }
    } else if report.len() == NAVICO_BEACON_BR24_SIZE {
        match deserialize::<BR24Beacon>(report) {
            Ok(data) => {
                if let Some(serial_no) = c_string(&data.serial_no) {
                    let radar_addr: SocketAddrV4 = data.radar_addr.into();

                    let radar_data: SocketAddrV4 = data.data.into();
                    let radar_report: SocketAddrV4 = data.report.into();
                    let radar_send: SocketAddrV4 = data.send.into();
                    let location_info: RadarLocationInfo = RadarLocationInfo::new(
                        "Navico",
                        Some("BR24"),
                        Some(serial_no),
                        None,
                        radar_addr.into(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, shutdown).await.unwrap();
                }
            }
            Err(e) => {
                error!("Failed to decode BR24 data: {}", e);
            }
        }
    }
    Ok(())
}

struct NavicoLocator {
    buf: Vec<u8>,
    buf_report: Vec<u8>,
    sock: Option<UdpSocket>,
    sock_report: Option<UdpSocket>,
}

impl NavicoLocator {
    async fn start(&mut self) -> io::Result<()> {
        match join_multicast(&NAVICO_BEACON_ADDRESS).await {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "Listening on {} for all Navico radars",
                    NAVICO_BEACON_ADDRESS
                );
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!("Beacon multicast failed: {}", e);
                return Err(e);
            }
        };
        match join_multicast(&NAVICO_COMMON_REPORT_ADDRESS).await {
            Ok(sock) => {
                self.sock_report = Some(sock);
                debug!(
                    "Listening on {} for BR24..4G Navico radars",
                    NAVICO_COMMON_REPORT_ADDRESS
                );
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!("Report multicast failed: {}", e);
                return Err(e);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl RadarLocator for NavicoLocator {
    async fn process_beacons(
        &mut self,
        radars: Arc<RwLock<Radars>>,
        shutdown: Shutdown,
    ) -> io::Result<()> {
        loop {
            if !self.sock.is_some() || !self.sock_report.is_some() {
                if let Err(_) = self.start().await {
                    continue;
                }
            }

            self.buf.clear();
            self.buf_report.clear();
            match (&self.sock, &self.sock_report) {
                (Some(sock), Some(sock_report)) => {
                    tokio::select! {
                        _ = shutdown.handle() => {
                            return Ok(());
                        }
                        result = sock.recv_buf_from(&mut self.buf) => {
                            match result {
                                            Ok((_len, from)) => {
                                                debug!("{} UDP recv {}: {:02X?}", from, self.buf.len(), &self.buf);
                                                process_report(&self.buf, from, &radars, &shutdown)
                                                    .await
                                                    .unwrap();
                                            }
                                            Err(e) => {
                                                debug!("Beacon read failed: {}", e);
                                                self.sock = None;
                                            }
                                        }
                        },
                        result = sock_report.recv_buf_from(&mut self.buf_report) => {
                            match result {
                                            Ok((_len, from)) => {
                                                debug!("{} UDP recv {}: {:02X?}", from, self.buf_report.len(), &self.buf_report);
                                                process_report(&self.buf_report, from, &radars, &shutdown)
                                                    .await
                                                    .unwrap();
                                            }
                                            Err(e) => {
                                                debug!("Beacon read failed: {}", e);
                                                self.sock = None;
                                            }
                                        }
                        },
                    }
                }
                _ => {
                    // Nothing to do
                }
            }
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = NavicoLocator {
        buf: Vec::with_capacity(2048),
        sock: None,
        buf_report: Vec::with_capacity(2048),
        sock_report: None,
    };
    Box::new(locator)
}

struct NavicoBR24Locator {
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
}

impl NavicoBR24Locator {
    async fn start(&mut self) -> io::Result<()> {
        match join_multicast(&NAVICO_BR24_BEACON_ADDRESS).await {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "Listening on {} for Navico BR24",
                    NAVICO_BR24_BEACON_ADDRESS
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
impl RadarLocator for NavicoBR24Locator {
    async fn process_beacons(
        &mut self,
        radars: Arc<RwLock<Radars>>,
        shutdown: Shutdown,
    ) -> io::Result<()> {
        self.start().await.unwrap();
        loop {
            match &self.sock {
                Some(sock) => {
                    self.buf.clear();
                    match sock.recv_buf_from(&mut self.buf).await {
                        Ok((_len, from)) => {
                            process_report(&self.buf, from, &radars, &shutdown)
                                .await
                                .unwrap();
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

pub fn create_br24_locator() -> Box<dyn RadarLocator + Send> {
    let locator = NavicoBR24Locator {
        buf: Vec::with_capacity(2048),
        sock: None,
    };
    Box::new(locator)
}
