use anyhow::{bail, Error};
use async_trait::async_trait;
use bincode::deserialize;
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator, RadarLocatorState};
use crate::network::LittleEndianSocketAddrV4;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{c_string, PrintableSlice};
use crate::{Brand, Session};

mod command;
mod report;
mod settings;

const RD_SPOKES_PER_REVOLUTION: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
const RD_SPOKE_LEN: usize = 1024;

const QUANTUM_SPOKES_PER_REVOLUTION: usize = 250;
const QUANTUM_SPOKE_LEN: usize = 252;

const NON_HD_PIXEL_VALUES: u8 = 16; // Old radars have one nibble
const HD_PIXEL_VALUES_RAW: u16 = 256; // New radars have one byte pixels
const HD_PIXEL_VALUES: u8 = 128; // ... but we drop the last bit so we have space for other data

const RAYMARINE_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 5800);
const RAYMARINE_QUANTUM_WIFI_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(232, 1, 1, 1)), 5800);

#[derive(Clone, Debug)]
struct RaymarineModel {
    model: BaseModel,
    hd: bool, // true if HD = 256 bits per pixel
    spokes_per_revolution: usize,
    max_spoke_len: usize, // 1024 for analog, 256 for Quantum?
    doppler: bool,        // true if Doppler is supported
    name: &'static str,
}

impl RaymarineModel {
    fn new_eseries() -> Self {
        RaymarineModel {
            model: BaseModel::RD,
            hd: false,
            spokes_per_revolution: RD_SPOKES_PER_REVOLUTION,
            max_spoke_len: 512,
            doppler: false,
            name: "E series Classic",
        }
    }

    fn try_into(model: &str) -> Option<Self> {
        let (model, hd, spokes_per_revolution, max_spoke_len, doppler, name) = match model {
            // All "E" strings derived from the raymarine.app.box.com EU declaration of conformity documents
            // Quantum models, believed working
            "E70210" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                false,
                "Quantum Q24",
            ),
            "E70344" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                false,
                "Quantum Q24C",
            ),
            "E70498" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                true,
                "Quantum Q24D",
            ),
            // Cyclone and Cyclone Pro models, untested, assume works as Quantum
            // Probably supports higher resulution though...
            "E70620" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                true,
                "Cyclone",
            ),
            "E70621" => (
                BaseModel::Quantum,
                true,
                QUANTUM_SPOKES_PER_REVOLUTION,
                QUANTUM_SPOKE_LEN,
                true,
                "Cyclone Pro",
            ),
            // Magnum, untested, assume works as RD
            "E70484" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Magnum 4kW",
            ),
            "E70487" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Magnum 12kW",
            ),
            // Open Array HD and SHD, introduced circa 2007
            "E52069" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Open Array HD 4kW",
            ),
            "E92160" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Open Array HD 12kW",
            ),
            "E52081" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Open Array SHD 4kW",
            ),
            "E52082" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "Open Array SHD 12kW",
            ),
            // And the actual RD models, introduced circa 2004
            "E92142" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "RD418HD",
            ),
            "E92143" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                RD_SPOKE_LEN,
                false,
                "RD424HD",
            ),
            "E92130" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                512,
                false,
                "RD418D",
            ),
            "E92132" => (
                BaseModel::RD,
                true,
                RD_SPOKES_PER_REVOLUTION,
                512,
                false,
                "RD424D",
            ),

            _ => return None,
        };
        Some(RaymarineModel {
            model,
            hd,
            spokes_per_revolution,
            max_spoke_len,
            doppler,
            name,
        })
    }
}

fn hd_to_pixel_values(hd: bool) -> u8 {
    if hd {
        HD_PIXEL_VALUES
    } else {
        NON_HD_PIXEL_VALUES
    }
}

/*
Let's take a look at what Raymarine radars send in their beacons.
First of all, it looks as if all ethernet devices send a beacon of length 56 bytes,
and that they also send a beacon of length 36 bytes.

The observation so far is that the 56 byte beacon contains a 4 byte "link_id" field,
which the next 36 byte beacon also contains.

We put them in a map for now, but probably we only need to store the last one.
 */

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineBeacon36 {
    beacon_type: [u8; 4],              // 0: always 0
    link_id: [u8; 4],                  // 4
    subtype: [u8; 4],                  // 8
    _field5: [u8; 4],                  // 12
    _field6: [u8; 4],                  // 16
    report: LittleEndianSocketAddrV4,  // 20
    _align1: [u8; 2],                  // 26
    command: LittleEndianSocketAddrV4, // 28
    _align2: [u8; 2],                  // 34
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineBeacon56 {
    beacon_type: [u8; 4], // 0: always 1
    subtype: [u8; 4],     // 4
    link_id: [u8; 4],     // 8
    _field4: [u8; 4],     // 12
    _field5: [u8; 4],     // 16
    model_name: [u8; 32], // 20: String like "QuantumRadar" when subtype = 0x66
    _field7: [u8; 4],     // 52
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum BaseModel {
    RD,
    Quantum,
}

impl fmt::Display for BaseModel {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            BaseModel::RD => "RD",
            BaseModel::Quantum => "Quantum",
        };
        write!(f, "{}", s)
    }
}

type LinkId = u32;

#[derive(Clone)]
struct RadarState {
    model_name: Option<String>,
    model: BaseModel,
}

#[derive(Clone)]
struct RaymarineLocatorState {
    session: Session,
    ids: HashMap<LinkId, RadarState>,
}

impl RaymarineLocatorState {
    fn new(session: Session) -> Self {
        RaymarineLocatorState {
            session,
            ids: HashMap::new(),
        }
    }

    fn process_beacon_36_report(
        &mut self,
        report: &[u8],
        from: &Ipv4Addr,
    ) -> Result<Option<RadarInfo>, Error> {
        match deserialize::<RaymarineBeacon36>(report) {
            Ok(data) => {
                let beacon_type = u32::from_le_bytes(data.beacon_type);
                if beacon_type != 0 {
                    log::warn!(
                        "{}: Raymarine 36 report: unexpected beacon type {}",
                        from,
                        beacon_type
                    );
                    return Ok(None);
                }

                let link_id = u32::from_le_bytes(data.link_id);

                if let Some(info) = self.ids.get(&link_id) {
                    log::debug!(
                        "{}: link {:08X} report: {:02X?} model {}",
                        from,
                        link_id,
                        report,
                        info.model
                    );
                    log::trace!("{}: data {:?}", from, data);

                    let model = info.model;
                    let subtype = u32::from_le_bytes(data.subtype);

                    match model {
                        BaseModel::Quantum => {
                            if subtype != 0x28 {
                                log::warn!(
                                    "{}: Raymarine 36 report: unexpected subtype {} for Quantum",
                                    from,
                                    subtype
                                );
                                return Ok(None);
                            }
                        }
                        BaseModel::RD => {
                            match subtype {
                                0x01 => {} // Continue
                                8 | 21 | 26 | 27 | 30 | 35 => {
                                    // Known unknowns
                                    return Ok(None);
                                }
                                _ => {
                                    log::warn!(
                                        "{}: Raymarine 36 report: unexpected subtype {} for RD",
                                        from,
                                        subtype
                                    );
                                    return Ok(None);
                                }
                            }
                        }
                    }
                    let doppler = false; // Improved later when model is known better

                    let (spokes_per_revolution, max_spoke_len) = match model {
                        BaseModel::Quantum => (QUANTUM_SPOKES_PER_REVOLUTION, QUANTUM_SPOKE_LEN),
                        BaseModel::RD => (RD_SPOKES_PER_REVOLUTION, RD_SPOKE_LEN),
                    };

                    let radar_addr: SocketAddrV4 = data.report.into();
                    let radar_send: SocketAddrV4 = data.command.into();
                    let location_info: RadarInfo = RadarInfo::new(
                        self.session.clone(),
                        LocatorId::Raymarine,
                        Brand::Raymarine,
                        None,
                        None,
                        16,
                        spokes_per_revolution,
                        max_spoke_len,
                        radar_addr.into(),
                        from.clone(),
                        radar_addr.into(),
                        radar_addr.into(),
                        radar_send.into(),
                        settings::new(self.session.clone(), info.model),
                        doppler,
                    );

                    return Ok(Some(location_info));
                } else {
                    log::trace!(
                        "{}: Raymarine 36 report: link_id {:08X} not found in ids: {:02X?}",
                        from,
                        link_id,
                        report
                    );
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(None)
    }

    fn process_beacon_56_report(&mut self, report: &[u8], from: &Ipv4Addr) -> Result<(), Error> {
        match deserialize::<RaymarineBeacon56>(report) {
            Ok(data) => {
                let beacon_type = u32::from_le_bytes(data.beacon_type);
                if beacon_type != 0x01 {
                    log::warn!(
                        "{}: Raymarine 56 report: unexpected beacon type {}",
                        from,
                        beacon_type
                    );
                    return Ok(());
                }

                let link_id = u32::from_le_bytes(data.link_id);
                let subtype = u32::from_le_bytes(data.subtype);

                match subtype {
                    0x66 => {
                        let model = BaseModel::Quantum;
                        let model_name: Option<String> =
                            c_string(&data.model_name).map(String::from);

                        if self
                            .ids
                            .insert(
                                link_id,
                                RadarState {
                                    model_name: model_name.clone(),
                                    model,
                                },
                            )
                            .is_none()
                        {
                            log::debug!(
                                "{}: Quantum located via report: {:02X?} len {}",
                                from,
                                report,
                                report.len()
                            );
                            log::debug!(
                                "{}: Quantum located via report: {} len {}",
                                from,
                                PrintableSlice::new(report),
                                report.len()
                            );
                            log::debug!(
                                "{}: link_id {:08X} model_name: {:?} model {}",
                                from,
                                link_id,
                                model_name,
                                model
                            );
                            log::debug!("{}: data {:?}", from, data);
                        }
                    }
                    0x01 => {
                        let model = BaseModel::RD;
                        let model_name = Some(model.to_string());

                        if self
                            .ids
                            .insert(
                                link_id,
                                RadarState {
                                    model_name: model_name.clone(),
                                    model,
                                },
                            )
                            .is_none()
                        {
                            log::debug!(
                                "{}: RD located via report: {:02X?} len {}",
                                from,
                                report,
                                report.len()
                            );
                            log::debug!(
                                "{}: link_id: {:08X} model_name: {:?} model {}",
                                from,
                                link_id,
                                model_name,
                                model
                            );
                        }
                    }
                    0x4d => {
                        // This is some sort of Wireless version (Quantum_W3)
                    }
                    0x11 => {
                        // Request from an MFD, ignore it
                    }
                    _ => {
                        // log::warn!("{}: Raymarine 56 report: unknown subtype {}", from, subtype);
                    }
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(())
    }

    fn found(&self, info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
        info.controls
            .set_string(&crate::settings::ControlType::UserName, info.key())
            .unwrap();

        if let Some(info) = radars.located(info) {
            // It's new, start the RadarProcessor thread

            if self.session.read().unwrap().args.output {
                let info_clone2 = info.clone();

                subsys.start(SubsystemBuilder::new("stdout", move |s| {
                    info_clone2.forward_output(s)
                }));
            }

            let report_name = info.key();
            let info_clone = info.clone();
            let report_receiver = report::RaymarineReportReceiver::new(
                self.session.clone(),
                info_clone,
                radars.clone(),
            );

            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }
    }
}

impl RadarLocatorState for RaymarineLocatorState {
    fn process(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        if report.len() < 2 {
            return Ok(());
        }

        log::trace!(
            "{}: Raymarine report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        log::trace!("{}: printable:     {}", from, PrintableSlice::new(report));

        match report.len() {
            36 => {
                // Common Raymarine message

                match Self::process_beacon_36_report(self, report, nic_addr) {
                    Ok(Some(info)) => {
                        self.found(info, radars, subsys);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::error!("{}: Error processing beacon: {}", from, e);
                    }
                }
            }
            56 => match Self::process_beacon_56_report(self, report, nic_addr) {
                Ok(()) => {}

                Err(e) => {
                    log::error!("{}: Error processing beacon: {}", from, e);
                }
            },
            _ => {
                log::trace!(
                    "{}: Unknown Raymarine report length: {}",
                    from,
                    report.len()
                );
            }
        }

        Ok(())
    }

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(Clone::clone(self))
    }
}

#[derive(Clone)]
struct RaymarineLocator {
    session: Session,
}

const RAYMARINE_MFD_BEACON: [u8; 56] = [
    0x01, 0x00, 0x00, 0x00, 0x11, 0x00, 0x00, 0x00, 0x38, 0x8c, 0x81, 0xd4, 0x6a, 0x01, 0x0e, 0x83,
    0x6c, 0x03, 0x12, 0xc6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x01, 0x00,
];

impl RadarLocator for RaymarineLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
            let beacon_address = if self.session.args().allow_wifi {
                &RAYMARINE_QUANTUM_WIFI_ADDRESS
            } else {
                &RAYMARINE_BEACON_ADDRESS
            };

            addresses.push(LocatorAddress::new(
                LocatorId::Raymarine,
                beacon_address,
                Brand::Raymarine,
                vec![&RAYMARINE_MFD_BEACON],
                Box::new(RaymarineLocatorState::new(self.session.clone())),
            ));
        }
    }
}

pub fn create_locator(session: Session) -> Box<dyn RadarLocator + Send> {
    let locator = RaymarineLocator { session };
    Box::new(locator)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use crate::brand::raymarine::RaymarineLocatorState;

    #[test]
    fn decode_raymarine_locator_beacon() {
        let session = crate::Session::new_fake();

        const VIA: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

        // This is a real beacon message from a Raymarine Quantum radar (E704980880217-NewZealand)
        // File "radar transmitting with range changes.pcap.gz"
        // Radar sends from 198.18.6.214 to 224.0.0.1:5800
        // packets of length 36, 56 and 70.
        // Spoke data seems to be on 232.1.243.1:2574
        const DATA1_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0x28, 0x0, 0x0, 0x0, 0x3, 0x0, 0x64, 0x0,
            0x6, 0x8, 0x10, 0x0, 0x1, 0xf3, 0x1, 0xe8, 0xe, 0xa, 0x11, 0x0, 0xd6, 0x6, 0x12, 0xc6,
            0xf, 0xa, 0x36, 0x0,
        ];
        const DATA1_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, 0x66, 0x0, 0x0, 0x0, 0x58, 0x6b, 0x80, 0xd6, 0xf3, 0x0, 0x0, 0x0,
            0xf3, 0x0, 0xa8, 0xc0, 0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x52, 0x61, 0x64,
            0x61, 0x72, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x2, 0x0, 0x0, 0x0,
        ];

        // The same radar transmitting via wired connection
        // Radar IP 10.30.200.221 sends to UDP 5800 a lot but only the messages
        // coming from port 5800 seem to be useful (sofar!)
        //
        const DATA2_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, // message_type
            0x58, 0x6b, 0x80, 0xd6, // link id
            0x28, 0x0, 0x0, 0x0, // submessage type
            0x3, 0x0, 0x64, 0x0, // ?
            0x6, 0x8, 0x10, 0x0, // ?
            0x1, 0xa7, 0x1, 0xe8, 0xe, 0xa, // 232.1.167.1:2574
            0x11, 0x0, // ?
            0xdd, 0xc8, 0x1e, 0xa, 0xf, 0xa, // 10.30.200.221:2575
            0x36, 0x0, // ?
        ];
        const DATA2_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, // message_type
            0x66, 0x0, 0x0, 0x0, // subtype?
            0x58, 0x6b, 0x80, 0xd6, // link id
            0xf3, 0x0, 0x0, 0x0, //
            0xa7, 0x27, 0xa8, 0xc0, //
            0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, // "Quantum"
            0x52, 0x61, 0x64, 0x61, 0x72, // "Radar"
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, // remaining blank bytes fills 32 bytes
            0x2, 0x0, 0x0, 0x0,
        ];

        // Analog radar connected to Eseries MFD
        // MFD IP addr 10.0.234.47
        const DATA3_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, // message_type
            0xb1, 0x69, 0xc2, 0xb2, // link_id
            0x1, 0x0, 0x0, 0x0, // sub_type 1
            0x1, 0x0, 0x1e, 0x0, //
            0xb, 0x8, 0x10, 0x0, //
            231, 69, 29, 224, 0x6, 0xa, 0x0, 0x0, // 224.29.69.231:2566 The radar sends to ...
            47, 234, 0, 10, 11, 8, 0, 0, // 10.0.234.47:2059 ... and receives on
        ];
        const DATA3_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, // message_type
            0x1, 0x0, 0x0, 0x0, // sub_type
            0xb1, 0x69, 0xc2, 0xb2, // link_id
            0xb, 0x2, 0x0, 0x0, //
            0x2f, 0xea, 0x0, 0xa, 0x0, //
            // From here on lots of ascii number (3 = 0x33) and 0xcc ...
            0x31, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x33, 0x4e, 0x37, 0xcc, 0x27, 0xcc,
            0x33, 0xcc, 0x33, 0xcc, 0x33, 0xcc, 0x30, 0xcc, 0x13, 0xc8, 0x33, 0xcc, 0x13, 0xcc,
            0x33, 0xc0, 0x13, 0x2, 0x0, 0x1, 0x0,
        ];

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA1_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA1_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("QuantumRadar".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(198, 18, 6, 214), 2575)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 243, 1), 2574)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 243, 1), 2574)
        );

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA2_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA2_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("QuantumRadar".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 30, 200, 221), 2575)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 167, 1), 2574)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(232, 1, 167, 1), 2574)
        );

        let mut state = RaymarineLocatorState::new(session.clone());
        let r = state.process_beacon_36_report(&DATA3_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_none());
        let r = state.process_beacon_56_report(&DATA3_56, &VIA);
        assert!(r.is_ok());
        let r = state.process_beacon_36_report(&DATA3_36, &VIA);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        log::debug!("Radar: {:?}", r);
        assert_eq!(r.controls.model_name(), Some("RD/HD/Eseries".to_string()));
        assert_eq!(r.serial_no, None);
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 0, 234, 47), 2059)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 29, 69, 231), 2566)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 29, 69, 231), 2566)
        );
    }
}
