use bincode::deserialize;
use log::{debug, error, log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator, RadarLocatorState};
use crate::network::NetworkSocketAddrV4;
use crate::radar::{RadarInfo, SharedRadars};
use crate::settings::ControlType;
use crate::util::c_string;
use crate::util::PrintableSlice;
use crate::{Brand, Session};

mod command;
mod data;
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
            0x0e => Model::BR24, // Davy's NorthStart BR24 from 2009
            0x0f => Model::BR24,
            0x08 => Model::Gen3,
            0x01 => Model::Gen4,
            0x00 => Model::HALO,
            _ => Model::Unknown,
        }
    }
}

const NAVICO_BEACON_SINGLE_SIZE: usize = size_of::<NavicoBeaconSingle>();
const NAVICO_BEACON_DUAL_SIZE: usize = size_of::<NavicoBeaconDual>();
const NAVICO_BEACON_BR24_SIZE: usize = size_of::<BR24Beacon>();

#[derive(Clone)]
struct NavicoLocatorState {
    session: Session,
}

impl NavicoLocatorState {
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
            trace!("Radar address request packet from {}", from);
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
                            self.session.clone(),
                            LocatorId::Gen3Plus,
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
                            settings::new(self.session.clone(), None),
                            true,
                        );
                        self.found(location_info, radars, subsys);

                        let radar_data: SocketAddrV4 = data.b.data.into();
                        let radar_report: SocketAddrV4 = data.b.report.into();
                        let radar_send: SocketAddrV4 = data.b.send.into();
                        let location_info: RadarInfo = RadarInfo::new(
                            self.session.clone(),
                            LocatorId::Gen3Plus,
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
                            settings::new(self.session.clone(), None),
                            true,
                        );
                        self.found(location_info, radars, subsys);
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
                            self.session.clone(),
                            LocatorId::Gen3Plus,
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
                            settings::new(self.session.clone(), None),
                            false,
                        );
                        self.found(location_info, radars, subsys);
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
                            self.session.clone(),
                            LocatorId::GenBR24,
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
                            settings::new(self.session.clone(), Some(BR24_MODEL_NAME)),
                            false,
                        );
                        self.found(location_info, radars, subsys);
                    }
                }
                Err(e) => {
                    error!("{} via {}: Failed to decode BR24 data: {}", from, via, e);
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
                info.set_legend(model == Model::HALO);
                radars.update(&info);
            }

            // Clone everything moved into future twice or more
            let data_name = info.key() + " data";
            let report_name = info.key() + " reports";
            let info_clone = info.clone();

            if self.session.read().unwrap().args.output {
                let info_clone2 = info.clone();

                subsys.start(SubsystemBuilder::new("stdout", move |s| {
                    info_clone2.forward_output(s)
                }));
            }

            let data_receiver = data::NavicoDataReceiver::new(self.session.clone(), info);
            let report_receiver = report::NavicoReportReceiver::new(
                self.session.clone(),
                info_clone,
                radars.clone(),
                model,
            );

            subsys.start(SubsystemBuilder::new(
                data_name,
                move |s: SubsystemHandle| data_receiver.run(s),
            ));
            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

impl RadarLocatorState for NavicoLocatorState {
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

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(NavicoLocatorState {
            session: self.session.clone(),
        }) // Navico is stateless
    }
}

#[derive(Clone)]
struct NavicoLocator {
    session: Session,
}

impl RadarLocator for NavicoLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        let mut beacon_request_packets: Vec<&'static [u8]> = Vec::new();
        if !self.session.read().unwrap().args.replay {
            beacon_request_packets.push(&NAVICO_ADDRESS_REQUEST_PACKET);
        };
        if !addresses.iter().any(|i| i.id == LocatorId::Gen3Plus) {
            addresses.push(LocatorAddress::new(
                LocatorId::Gen3Plus,
                &NAVICO_BEACON_ADDRESS,
                Brand::Navico,
                beacon_request_packets,
                Box::new(NavicoLocatorState {
                    session: self.session.clone(),
                }),
            ));
        }
    }
}

pub fn create_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = NavicoLocator { session };
    Box::new(locator)
}

#[derive(Clone)]
struct NavicoBR24Locator {
    session: Session,
}

impl RadarLocator for NavicoBR24Locator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::GenBR24) {
            addresses.push(LocatorAddress::new(
                LocatorId::GenBR24,
                &NAVICO_BR24_BEACON_ADDRESS,
                Brand::Navico,
                vec![&NAVICO_ADDRESS_REQUEST_PACKET],
                Box::new(NavicoLocatorState {
                    session: self.session.clone(),
                }),
            ));
        }
    }
}

pub fn create_br24_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = NavicoBR24Locator { session };
    Box::new(locator)
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
