use anyhow::{bail, Error};
use async_trait::async_trait;
use bincode::deserialize;
use log::{log_enabled, trace};
use serde::Deserialize;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator, RadarLocatorState};
use crate::network::LittleEndianSocketAddrV4;
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{c_string, PrintableSlice};
use crate::GLOBAL_ARGS;

// mod command;
// mod data;
// mod report;
mod settings;

const ESERIES_SPOKES: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
const ESERIES_SPOKE_LEN: usize = 1024;

const QUANTUM_SPOKES: usize = 250;
const QUANTUM_SPOKE_LEN: usize = 252;

const RAYMARINE_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 5800);

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineBeacon36 {
    beacon_type: u32,                // 0: always 0
    link_id: u32,                    // 4
    subtype: u32,                    // byte 8
    _field5: u32,                    // 12
    _field6: u32,                    // 16
    data: LittleEndianSocketAddrV4,  // 20
    _empty1: u16,                    // 26
    radar: LittleEndianSocketAddrV4, // 28
    _empty2: u16,                    // 34
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineBeacon56 {
    beacon_type: u32,     // 0: always 1
    subtype: u32,         // 4
    link_id: u32,         // 8
    _field4: u32,         // 12
    _field5: u32,         // 16
    model_name: [u8; 32], // 20: String like "QuantumRadar" when subtype = 0x66
    _field7: u32,         // 52
}

#[derive(Copy, Clone, Debug)]
pub enum Model {
    Eseries,
    Quantum,
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Eseries => "RD/HD/Eseries",
            Model::Quantum => "Quantum",
        };
        write!(f, "{}", s)
    }
}

impl Model {
    pub fn new(s: &str) -> Self {
        match s {
            "Quantum" => Model::Quantum,
            _ => Model::Eseries,
        }
    }
}

fn found(info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    info.controls
        .set_string(&crate::settings::ControlType::UserName, info.key())
        .unwrap();

    if let Some(mut info) = radars.located(info) {
        // It's new, start the RadarProcessor thread

        // Load the model name afresh, it may have been modified from persisted data
        let model = match info.controls.model_name() {
            Some(s) => Model::new(&s),
            None => Model::Eseries,
        };
        let info2 = info.clone();
        settings::update_when_model_known(&mut info.controls, model, &info2);
        let doppler_supported = false;
        info.set_legend(doppler_supported);
        radars.update(&info);

        // Clone everything moved into future twice or more
        if GLOBAL_ARGS.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        // let data_name = info.key() + " data";
        // let report_name = info.key() + " reports";
        // let info_clone = info.clone();
        // let (tx_data, rx_data) = mpsc::channel(10);
        // let data_receiver = data::RaymarineDataReceiver::new(info, rx_data, args.replay);
        // let report_receiver =
        //     report::RaymarineReportReceiver::new(info_clone, radars.clone(), model, tx_data);

        // subsys.start(SubsystemBuilder::new(
        //     data_name,
        //     move |s: SubsystemHandle| data_receiver.run(s),
        // ));
        // subsys.start(SubsystemBuilder::new(report_name, |s| {
        //     report_receiver.run(s)
        // }));
    }
}

type LinkId = u32;

#[derive(Clone)]
struct RadarState {
    model_name: Option<String>,
    model: Model,
}

#[derive(Clone)]
struct RaymarineLocatorState {
    ids: HashMap<LinkId, RadarState>,
}

impl RaymarineLocatorState {
    fn new() -> Self {
        RaymarineLocatorState {
            ids: HashMap::new(),
        }
    }

    fn process_beacon_36_report(
        &mut self,
        report: &[u8],
        via: &Ipv4Addr,
    ) -> Result<Option<RadarInfo>, Error> {
        match deserialize::<RaymarineBeacon36>(report) {
            Ok(data) => {
                if data.beacon_type != 0 {
                    return Ok(None);
                }

                let link_id = data.link_id;

                if let Some(info) = self.ids.get(&link_id) {
                    let model = info.model;
                    match model {
                        Model::Quantum => {
                            if data.subtype != 0x28 {
                                return Ok(None);
                            }
                        }
                        Model::Eseries => {
                            if data.subtype != 0x01 {
                                return Ok(None);
                            }
                        }
                    }

                    let (spokes, max_spoke_len) = match model {
                        Model::Quantum => (QUANTUM_SPOKES, QUANTUM_SPOKE_LEN),
                        Model::Eseries => (ESERIES_SPOKES, ESERIES_SPOKE_LEN),
                    };

                    let radar_addr: SocketAddrV4 = data.data.into();
                    let radar_send: SocketAddrV4 = data.radar.into();
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Raymarine,
                        "Raymarine",
                        None,
                        None,
                        16,
                        spokes,
                        max_spoke_len,
                        radar_addr.into(),
                        via.clone(),
                        radar_addr.into(),
                        radar_addr.into(),
                        radar_send.into(),
                        settings::new(info.model_name.as_deref()),
                    );

                    return Ok(Some(location_info));
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(None)
    }

    fn process_beacon_56_report(&mut self, report: &[u8], _via: &Ipv4Addr) -> Result<(), Error> {
        match deserialize::<RaymarineBeacon56>(report) {
            Ok(data) => {
                if data.beacon_type != 0x01 {
                    return Ok(());
                }

                let link_id = data.link_id;

                match data.subtype {
                    0x66 => {
                        let model = Model::Quantum;
                        let model_name: Option<String> =
                            c_string(&data.model_name).map(String::from);
                        self.ids.insert(link_id, RadarState { model_name, model });
                    }
                    0x01 => {
                        let model = Model::Eseries;
                        let model_name = Some(model.to_string());
                        self.ids.insert(link_id, RadarState { model_name, model });
                    }
                    _ => {}
                }
            }
            Err(e) => {
                bail!(e);
            }
        }
        Ok(())
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

        if log_enabled!(log::Level::Trace) {
            trace!(
                "{}: Raymarine report: {:02X?} len {}",
                from,
                report,
                report.len()
            );
            trace!("{}: printable:     {}", from, PrintableSlice::new(report));
        }

        match report.len() {
            36 => {
                // Common Raymarine message

                match Self::process_beacon_36_report(self, report, nic_addr) {
                    Ok(Some(info)) => {
                        found(info, radars, subsys);
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
            _ => {}
        }

        Ok(())
    }

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(Clone::clone(self))
    }
}

struct RaymarineLocator {}

#[async_trait]
impl RadarLocator for RaymarineLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
            addresses.push(LocatorAddress::new(
                LocatorId::Raymarine,
                &RAYMARINE_BEACON_ADDRESS,
                "raymarine",
                None, // The Raymarine radars send the beacon reports by themselves, no polling needed
                Box::new(RaymarineLocatorState::new()),
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = RaymarineLocator {};
    Box::new(locator)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddrV4};

    use crate::brand::raymarine::RaymarineLocatorState;

    #[test]
    fn decode_raymarine_locator_beacon() {
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

        let mut state = RaymarineLocatorState::new();
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

        let mut state = RaymarineLocatorState::new();
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

        let mut state = RaymarineLocatorState::new();
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
