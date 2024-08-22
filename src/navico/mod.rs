use async_trait::async_trait;
use bincode::deserialize;
use crossbeam::atomic::AtomicCell;
use enum_primitive_derive::Primitive;
use log::{debug, error, log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, RwLock};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorId, RadarListenAddress, RadarLocator};
use crate::radar::{located, DopplerMode, RadarInfo, Radars};
use crate::util::c_string;
use crate::util::PrintableSlice;

mod command;
mod data;
mod report;

const NAVICO_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 5)), 6878);

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

/* NAVICO API SPOKES */
/*
 * Data coming from radar is always 4 bits, packed two per byte.
 * The values 14 and 15 may be special depending on DopplerMode (only on HALO).
 *
 * To support targets, target trails and doppler we map those values 0..15 to
 * a
 */

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

const NAVICO_ADDRESS_REQUEST_PACKET: [u8; 2] = [0x01, 0xB1];

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
    report: NetworkSocketAddrV4,
    _filler5: [u8; 4],
    send: NetworkSocketAddrV4,
    _filler6: [u8; 4],
    data: NetworkSocketAddrV4, // Note different order from newer radars
}

#[derive(Copy, Clone, PartialEq, Debug, Primitive)]
enum Model {
    Unknown = 0xff,
    BR24 = 0x0f,
    Gen3 = 0x08,
    Gen4 = 0x01,
    HALO = 0x00,
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Unknown => "",
            Model::BR24 => "BR24",
            Model::Gen3 => "3G",
            Model::Gen4 => "4G",
            Model::HALO => "HALO",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug)]
pub struct NavicoSettings {
    radars: Arc<RwLock<Radars>>,
    doppler: AtomicCell<DopplerMode>,
    model: AtomicCell<Model>,
}

const NAVICO_BEACON_SINGLE_SIZE: usize = size_of::<NavicoBeaconSingle>();
const NAVICO_BEACON_DUAL_SIZE: usize = size_of::<NavicoBeaconDual>();
const NAVICO_BEACON_BR24_SIZE: usize = size_of::<BR24Beacon>();

fn found(info: RadarInfo, radars: &Arc<RwLock<Radars>>, subsys: &SubsystemHandle) {
    if let Some(info) = located(info, radars) {
        // It's new, start the RadarProcessor thread
        let navico_settings = Arc::new(NavicoSettings {
            radars: radars.clone(),
            doppler: AtomicCell::new(DopplerMode::None),
            model: AtomicCell::new(Model::Unknown),
        });

        let command_sender = command::Command::new(info.clone(), navico_settings.clone());

        // Clone everything moved into future twice or more
        let info_clone = info.clone();
        let navico_settings_clone = navico_settings.clone();

        let data_receiver = data::Receive::new(info, navico_settings);
        let report_receiver =
            report::Receive::new(info_clone, navico_settings_clone, command_sender);

        subsys.start(SubsystemBuilder::new("Navico Data Receiver", move |s| {
            data_receiver.run(s)
        }));
        subsys.start(SubsystemBuilder::new("Navico Report Receiver", |s| {
            report_receiver.run(s)
        }));
    }
}

fn process_locator_report(
    report: &[u8],
    from: &SocketAddr,
    via: &Ipv4Addr,
    radars: &Arc<RwLock<Radars>>,
    subsys: &SubsystemHandle,
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

    if report == NAVICO_ADDRESS_REQUEST_PACKET {
        debug!("Radar address request packet from {}", from);
        return Ok(());
    }
    if report[0] == 0x1 && report[1] == 0xB2 {
        // Common Navico message

        return process_beacon_report(report, from, via, radars, subsys);
    }
    Ok(())
}

fn process_beacon_report(
    report: &[u8],
    from: &SocketAddr,
    via: &Ipv4Addr,
    radars: &Arc<RwLock<Radars>>,
    subsys: &SubsystemHandle,
) -> Result<(), io::Error> {
    if report.len() < size_of::<BR24Beacon>() {
        debug!(
            "{} via {}: Incomplete beacon, length {}",
            from,
            via,
            report.len()
        );
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
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Gen3Plus,
                        "Navico",
                        None,
                        Some(serial_no),
                        Some("A"),
                        16,
                        2048,
                        1024,
                        radar_addr.into(),
                        via.clone(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, subsys);

                    let radar_data: SocketAddrV4 = data.b.data.into();
                    let radar_report: SocketAddrV4 = data.b.report.into();
                    let radar_send: SocketAddrV4 = data.b.send.into();
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Gen3Plus,
                        "Navico",
                        None,
                        Some(serial_no),
                        Some("B"),
                        16,
                        2048,
                        1024,
                        radar_addr.into(),
                        via.clone(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, subsys);
                }
            }
            Err(e) => {
                error!(
                    "{} via {}: Failed to decode dual range data: {}",
                    from, via, e
                );
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
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Gen3Plus,
                        "Navico",
                        None,
                        Some(serial_no),
                        None,
                        16,
                        2048,
                        1024,
                        radar_addr.into(),
                        via.clone(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, subsys);
                }
            }
            Err(e) => {
                error!(
                    "{} via {}: Failed to decode single range data: {}",
                    from, via, e
                );
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
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::GenBR24,
                        "Navico",
                        Some("BR24"),
                        Some(serial_no),
                        None,
                        16,
                        2048,
                        1024,
                        radar_addr.into(),
                        via.clone(),
                        radar_data.into(),
                        radar_report.into(),
                        radar_send.into(),
                    );
                    found(location_info, radars, subsys);
                }
            }
            Err(e) => {
                error!("{} via {}: Failed to decode BR24 data: {}", from, via, e);
            }
        }
    }
    Ok(())
}

struct NavicoLocator {}

#[async_trait]
impl RadarLocator for NavicoLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>) {
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::Gen3Plus && i.brand == "Navico Beacon")
        {
            addresses.push(RadarListenAddress::new(
                LocatorId::Gen3Plus,
                &NAVICO_BEACON_ADDRESS,
                "Navico Beacon",
                Some(&NAVICO_ADDRESS_REQUEST_PACKET),
                &process_locator_report,
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = NavicoLocator {};
    Box::new(locator)
}

struct NavicoBR24Locator {}

impl RadarLocator for NavicoBR24Locator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>) {
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::GenBR24 && i.brand == "Navico BR24 Beacon")
        {
            addresses.push(RadarListenAddress::new(
                LocatorId::GenBR24,
                &NAVICO_BR24_BEACON_ADDRESS,
                "Navico BR24 Beacon",
                Some(&NAVICO_ADDRESS_REQUEST_PACKET),
                &process_locator_report,
            ));
        }
    }
}

pub fn create_br24_locator() -> Box<dyn RadarLocator + Send> {
    let locator = NavicoBR24Locator {};
    Box::new(locator)
}
