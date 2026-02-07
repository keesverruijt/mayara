use bincode::deserialize;
use num_derive::{FromPrimitive, ToPrimitive};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use strum::VariantNames;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::LocatorAddress;
use crate::network::NetworkSocketAddrV4;
use crate::radar::{RadarInfo, SharedRadars};
use crate::settings::ControlType;
use crate::util::PrintableSlice;
use crate::util::c_string;
use crate::{Brand, Cli};

use super::{LocatorId, RadarLocator};

mod command;
mod info;
mod report;
mod settings;

const NAVICO_SPOKES: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
const NAVICO_SPOKE_LEN: usize = 1024;

// Spoke numbers go from [0..4096>, but only half of them are used.
// The actual image is 2048 x 1024 x 4 bits
const NAVICO_SPOKES_RAW: u16 = 4096;
const NAVICO_BITS_PER_PIXEL: usize = BITS_PER_NIBBLE;

const SPOKES_PER_FRAME: usize = 32;
const BITS_PER_BYTE: usize = 8;
const BITS_PER_NIBBLE: usize = 4;
const NAVICO_PIXELS_PER_BYTE: usize = BITS_PER_BYTE / NAVICO_BITS_PER_PIXEL;
const RADAR_LINE_DATA_LENGTH: usize = NAVICO_SPOKE_LEN / NAVICO_PIXELS_PER_BYTE;

const NAVICO_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 5)), 6878);
const NAVICO_INFO_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 238, 55, 73), 7527);
const NAVICO_SPEED_ADDRESS_A: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 20), 6690);
const NAVICO_SPEED_ADDRESS_B: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(236, 6, 7, 15), 6005);

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
const NAVICO_ADDRESS_REQUEST_PACKET: [u8; 2] = [0x01, 0xB1];

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconHeader {
    _id: u16,
    serial_no: [u8; 16],             // ASCII serial number, zero terminated
    radar_addr: NetworkSocketAddrV4, // 0A 00 43 D9 01 01 = DHCP address of radar
    _filler1: [u8; 12],              // 11000000
    _addr1: NetworkSocketAddrV4,     // EC0608201970 = 236.6.8.32 port 6512
    _filler2: [u8; 4],               // 11000000
    _addr2: NetworkSocketAddrV4,     // EC0607161A26 = 236.6.8.22 port 6694
    _filler3: [u8; 10],              // 1F002001020010000000
    _addr3: NetworkSocketAddrV4,     // EC0608211971 = 236.6.8.33 port 6513
    _filler4: [u8; 4],               // 11000000
    _addr4: NetworkSocketAddrV4,     // EC0608221972 = 236.6.8.34 port 6514
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconRadar {
    _filler1: [u8; 10],          // 10002001030010000000
    data: NetworkSocketAddrV4,   // EC0608231973 = 236.6.8.35 port 6515
    _filler2: [u8; 4],           // 11000000
    send: NetworkSocketAddrV4,   // EC0608241974 = 236.6.8.36 port 6516
    _filler3: [u8; 4],           // 12000000
    report: NetworkSocketAddrV4, // EC0608231975 = 236.6.7.35 port 6517
}

// Radars that have one internal radar: 3G, Halo 20, etc.

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconSingle {
    header: NavicoBeaconHeader,
    a: NavicoBeaconRadar,
}

// As seen on all dual radar (4G, HALO 20+, 24, 3, etc)
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct NavicoBeaconDual {
    header: NavicoBeaconHeader,
    a: NavicoBeaconRadar,
    b: NavicoBeaconRadar,
    /* We don't care about the rest: */
    /*
    _filler11: [u8; 10],           // 12002001030010000000
    addr11: NetworkSocketAddrV4,        // EC0608231979 = 236.6.8.35 port 6521
    _filler12: [u8; 4],            // 11000000
    addr12: NetworkSocketAddrV4,        // EC060827197A = 236.6.8.39 port 6522
    _filler13: [u8; 4],            // 12000000
    addr13: NetworkSocketAddrV4,        // EC060823197B = 236.6.8.35 port 6523
    _filler14: [u8; 10],           // 12002002030010000000
    addr14: NetworkSocketAddrV4,        // EC060825197C = 236.6.8.37 port 6524
    _filler15: [u8; 4],            // 11000000
    addr15: NetworkSocketAddrV4,        // EC060828197D = 236.6.8.40 port 6525
    _filler16: [u8; 4],            // 12000000
    addr16: NetworkSocketAddrV4,        // EC060825197E = 236.6.8.37 port 6526
    */
}

const NAVICO_BR24_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(236, 6, 7, 4)), 6768);

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

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum Model {
    Unknown,
    BR24,
    Gen3,
    Gen4,
    HALO,
    HaloOrG4,
}

const BR24_MODEL_NAME: &str = "BR24";

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Unknown => "",
            Model::BR24 => BR24_MODEL_NAME,
            Model::Gen3 => "3G",
            Model::Gen4 => "4G",
            Model::HALO => "HALO",
            Model::HaloOrG4 => "HALO or 4G",
        };
        write!(f, "{}", s)
    }
}

impl Model {
    pub fn new(s: &str) -> Self {
        match s {
            BR24_MODEL_NAME => Model::BR24,
            "3G" => Model::Gen3,
            "4G" => Model::Gen4,
            "HALO" => Model::HALO,
            _ => Model::Unknown,
        }
    }

    pub fn from(model: u8) -> Self {
        match model {
            0x0e => Model::BR24, // Davy's NorthStar BR24 from 2009
            0x0f => Model::BR24,
            0x08 => Model::Gen3,
            0x01 => Model::HaloOrG4, // New Firmware in 2025
            0x00 => Model::HALO,
            _ => Model::Unknown,
        }
    }
}

#[derive(PartialEq, FromPrimitive, ToPrimitive, VariantNames)]
enum HaloMode {
    Custom = 0,
    Harbor = 1,
    Offshore = 2,
    Buoy = 3,
    Weather = 4,
    Bird = 5,
}

/// There are some controls that turn read-only when the HaloMode is not Custom
const DYNAMIC_READ_ONLY_CONTROL_TYPES: [ControlType; 5] = [
    ControlType::NoiseRejection,
    ControlType::TargetExpansion,
    ControlType::TargetSeparation,
    ControlType::LocalInterferenceRejection,
    ControlType::ScanSpeed,
];

const NAVICO_BEACON_SINGLE_SIZE: usize = size_of::<NavicoBeaconSingle>();
const NAVICO_BEACON_DUAL_SIZE: usize = size_of::<NavicoBeaconDual>();
const NAVICO_BEACON_BR24_SIZE: usize = size_of::<BR24Beacon>();

#[derive(Clone)]
struct NavicoLocator {
    args: Cli,
}

impl NavicoLocator {
    fn process_locator_report(
        &self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < 2 {
            return Ok(());
        }

        log::trace!(
            "{}: Navico report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        log::trace!("{}: printable:     {}", from, PrintableSlice::new(report));

        if report == NAVICO_ADDRESS_REQUEST_PACKET {
            log::trace!("Radar address request packet from {}", from);
            return Ok(());
        }
        if report[0] == 0x1 && report[1] == 0xB2 {
            // Common Navico message

            return self.process_beacon_report(report, from, via, radars, subsys);
        }
        Ok(())
    }

    fn process_beacon_report(
        &self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        if report.len() < size_of::<BR24Beacon>() {
            log::debug!(
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
                    log::debug!("{} sent NavicoBeaconDual {:?}", from, data);
                    if let Some(serial_no) = c_string(&data.header.serial_no) {
                        let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                        let radar_data: SocketAddrV4 = data.a.data.into();
                        let radar_report: SocketAddrV4 = data.a.report.into();
                        let radar_send: SocketAddrV4 = data.a.send.into();
                        let location_info: RadarInfo = RadarInfo::new(
                            &self.args,
                            Brand::Navico,
                            Some(serial_no),
                            Some("A"),
                            16,
                            NAVICO_SPOKES,
                            NAVICO_SPOKE_LEN,
                            radar_addr.into(),
                            via.clone(),
                            radar_data.into(),
                            radar_report.into(),
                            radar_send.into(),
                            settings::new(&self.args, None),
                            true,
                        );
                        self.found(location_info, radars, subsys);

                        let radar_data: SocketAddrV4 = data.b.data.into();
                        let radar_report: SocketAddrV4 = data.b.report.into();
                        let radar_send: SocketAddrV4 = data.b.send.into();
                        let location_info: RadarInfo = RadarInfo::new(
                            &self.args,
                            Brand::Navico,
                            Some(serial_no),
                            Some("B"),
                            16,
                            NAVICO_SPOKES,
                            NAVICO_SPOKE_LEN,
                            radar_addr.into(),
                            via.clone(),
                            radar_data.into(),
                            radar_report.into(),
                            radar_send.into(),
                            settings::new(&self.args, None),
                            true,
                        );
                        self.found(location_info, radars, subsys);
                    }
                }
                Err(e) => {
                    log::error!(
                        "{} via {}: Failed to decode dual range capable data: {}",
                        from,
                        via,
                        e
                    );
                }
            }
        } else if report.len() >= NAVICO_BEACON_SINGLE_SIZE {
            match deserialize::<NavicoBeaconSingle>(report) {
                Ok(data) => {
                    log::debug!("{} sent NavicoBeaconSingle {:?}", from, data);
                    if let Some(serial_no) = c_string(&data.header.serial_no) {
                        let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                        let radar_data: SocketAddrV4 = data.a.data.into();
                        let radar_report: SocketAddrV4 = data.a.report.into();
                        let radar_send: SocketAddrV4 = data.a.send.into();
                        let location_info: RadarInfo = RadarInfo::new(
                            &self.args,
                            Brand::Navico,
                            Some(serial_no),
                            None,
                            16,
                            NAVICO_SPOKES,
                            NAVICO_SPOKE_LEN,
                            radar_addr.into(),
                            via.clone(),
                            radar_data.into(),
                            radar_report.into(),
                            radar_send.into(),
                            settings::new(&self.args, None),
                            false,
                        );
                        self.found(location_info, radars, subsys);
                    }
                }
                Err(e) => {
                    log::error!(
                        "{} via {}: Failed to decode single range capable data: {}",
                        from,
                        via,
                        e
                    );
                }
            }
        } else if report.len() == NAVICO_BEACON_BR24_SIZE {
            match deserialize::<BR24Beacon>(report) {
                Ok(data) => {
                    log::debug!("{} sent BR24Beacon {:?}", from, data);

                    if let Some(serial_no) = c_string(&data.serial_no) {
                        let radar_addr: SocketAddrV4 = data.radar_addr.into();

                        let radar_data: SocketAddrV4 = data.data.into();
                        let radar_report: SocketAddrV4 = data.report.into();
                        let radar_send: SocketAddrV4 = data.send.into();
                        let location_info: RadarInfo = RadarInfo::new(
                            &self.args,
                            Brand::Navico,
                            Some(serial_no),
                            None,
                            16,
                            NAVICO_SPOKES,
                            NAVICO_SPOKE_LEN,
                            radar_addr.into(),
                            via.clone(),
                            radar_data.into(),
                            radar_report.into(),
                            radar_send.into(),
                            settings::new(&self.args, Some(BR24_MODEL_NAME)),
                            false,
                        );
                        self.found(location_info, radars, subsys);
                    }
                }
                Err(e) => {
                    log::error!("{} via {}: Failed to decode BR24 data: {}", from, via, e);
                }
            }
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string(&crate::settings::ControlType::UserName, info.key())
            .unwrap();

        if let Some(mut info) = radars.located(info) {
            // It's new, start the RadarProcessor thread

            // Load the model name afresh, it may have been modified from persisted data
            let model = match info.controls.model_name() {
                Some(s) => Model::new(&s),
                None => Model::Unknown,
            };
            if model != Model::Unknown {
                let info2 = info.clone();
                settings::update_when_model_known(&mut info.controls, model, &info2);
                info.set_doppler(model == Model::HALO);
            }
            radars.update(&mut info);

            let report_name = info.key() + " reports";

            info.start_forwarding_radar_messages_to_stdout(&subsys);

            let report_receiver =
                report::NavicoReportReceiver::new(&self.args, info, radars.clone(), model);

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

impl RadarLocator for NavicoLocator {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        self.process_locator_report(message, from, nic_addr, radars, subsys)
    }

    fn clone(&self) -> Box<dyn RadarLocator> {
        Box::new(NavicoLocator {
            args: self.args.clone(),
        }) // Navico is stateless
    }
}

pub(super) fn new(args: &Cli, addresses: &mut Vec<LocatorAddress>) {
    if !addresses.iter().any(|i| i.id == LocatorId::Gen3Plus) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !args.replay {
            beacon_request_packets.push(&NAVICO_ADDRESS_REQUEST_PACKET);
        };
        addresses.push(LocatorAddress::new(
            LocatorId::Gen3Plus,
            &NAVICO_BEACON_ADDRESS,
            Brand::Navico,
            beacon_request_packets,
            Box::new(NavicoLocator { args: args.clone() }),
        ));
    }

    if !addresses.iter().any(|i| i.id == LocatorId::GenBR24) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !args.replay {
            beacon_request_packets.push(&NAVICO_ADDRESS_REQUEST_PACKET);
        };
        addresses.push(LocatorAddress::new(
            LocatorId::GenBR24,
            &NAVICO_BR24_BEACON_ADDRESS,
            Brand::Navico,
            beacon_request_packets,
            Box::new(NavicoLocator { args: args.clone() }),
        ));
    }
}

const BLANKING_SETS: [(usize, ControlType, ControlType); 4] = [
    (
        0,
        ControlType::NoTransmitStart1,
        ControlType::NoTransmitEnd1,
    ),
    (
        1,
        ControlType::NoTransmitStart2,
        ControlType::NoTransmitEnd2,
    ),
    (
        2,
        ControlType::NoTransmitStart3,
        ControlType::NoTransmitEnd3,
    ),
    (
        3,
        ControlType::NoTransmitStart4,
        ControlType::NoTransmitEnd4,
    ),
];
