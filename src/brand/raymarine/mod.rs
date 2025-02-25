use anyhow::{bail, Error};
use async_trait::async_trait;
use bincode::deserialize;
use enum_primitive_derive::Primitive;
use log::{log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator};
use crate::radar::{DopplerMode, Legend, RadarInfo, SharedRadars};
use crate::settings::ControlValue;
use crate::util::PrintableSlice;

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

// Messages sent to Data receiver
#[derive(Debug)]
pub enum DataUpdate {
    Doppler(DopplerMode),
    Legend(Legend),
    ControlValue(mpsc::Sender<ControlValue>, ControlValue),
}

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

// The beacon message from BR24 (and first gen 3G) is _slightly_ different,
// so it needs a different structure. It is also sent to a different MultiCast address!
#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineBeacon {
    _field1: u32,               // 0
    serial: [u8; 4],            // 4
    model_id: u8,               // byte 8
    _field3: u8,                // byte 9
    _field4: u16,               // byte 10
    _field5: u32,               // 12
    _field6: u32,               // 16
    data: NetworkSocketAddrV4,  // 20
    _empty1: u16,               // 26
    radar: NetworkSocketAddrV4, // 28
    _empty2: u16,               // 34
}

#[derive(Copy, Clone, PartialEq, Debug, Primitive)]
pub enum Model {
    Unknown = 0xff,
    Eseries = 0x01,
    Quantum1 = 0x23,
    Quantum = 0x28,
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            Model::Unknown => "",
            Model::Eseries => "E Series",
            Model::Quantum => "Quantum",
            Model::Quantum1 => "Quantum 1",
        };
        write!(f, "{}", s)
    }
}

impl Model {
    pub fn new(s: &str) -> Self {
        match s {
            "Quantum" => Model::Quantum,
            "Eseries" => Model::Eseries,
            "Quantum1" => Model::Quantum1,
            _ => Model::Unknown,
        }
    }
}

fn found(mut info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    info.set_string(&crate::settings::ControlType::UserName, info.key())
        .unwrap();

    if let Some(mut info) = radars.located(info) {
        // It's new, start the RadarProcessor thread

        // Load the model name afresh, it may have been modified from persisted data
        let model = match info.model_name() {
            Some(s) => Model::new(&s),
            None => Model::Unknown,
        };
        if model != Model::Unknown {
            let info2 = info.clone();
            settings::update_when_model_known(&mut info.controls, model, &info2);
            let doppler_supported = false;
            info.set_legend(doppler_supported);
            radars.update(&info);
        }

        // Clone everything moved into future twice or more
        let args = radars.cli_args();

        if args.output {
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

fn process_locator_report(
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
            "{}: Raymarine report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        trace!("{}: printable:     {}", from, PrintableSlice::new(report));
    }

    if report.len() == size_of::<RaymarineBeacon>() {
        // Common Raymarine message

        match process_beacon_report(report, via, radars.cli_args().replay) {
            Ok(Some(info)) => {
                found(info, radars, subsys);
            }
            Ok(None) => {}
            Err(e) => {
                log::error!("{}: Error processing beacon: {}", from, e);
            }
        }
    }

    Ok(())
}

fn process_beacon_report(
    report: &[u8],
    via: &Ipv4Addr,
    replay: bool,
) -> Result<Option<RadarInfo>, Error> {
    match deserialize::<RaymarineBeacon>(report) {
        Ok(data) => {
            let model: Result<Model, _> = data.model_id.try_into();
            match model {
                Err(_) => {
                    bail!("Unknown model # {}", report[8]);
                }
                Ok(model) => {
                    let (spokes, max_spoke_len) = match model {
                        Model::Quantum => (QUANTUM_SPOKES, QUANTUM_SPOKE_LEN),
                        Model::Quantum1 => (QUANTUM_SPOKES, QUANTUM_SPOKE_LEN),
                        Model::Eseries => (ESERIES_SPOKES, ESERIES_SPOKE_LEN),
                        _ => (0, 0),
                    };

                    let radar_addr: SocketAddrV4 = data.data.into();

                    let radar_send: SocketAddrV4 = data.radar.into();
                    let serial_no = u32::from_le_bytes(data.serial);
                    let serial_str = format!("{}", serial_no);
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Raymarine,
                        "Raymarine",
                        Some(serial_str.as_str()),
                        None,
                        16,
                        spokes,
                        max_spoke_len,
                        radar_addr.into(),
                        via.clone(),
                        radar_addr.into(),
                        radar_addr.into(),
                        radar_send.into(),
                        settings::new(None, replay),
                    );

                    Ok(Some(location_info))
                }
            }
        }
        Err(e) => {
            bail!(e);
        }
    }
}

struct RaymarineLocator {}

#[async_trait]
impl RadarLocator for RaymarineLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Raymarine) {
            addresses.push(LocatorAddress::new(
                LocatorId::Raymarine,
                &RAYMARINE_BEACON_ADDRESS,
                "Raymarine Beacon",
                None, // The Raymarine radars send the beacon reports by themselves, no polling needed
                &process_locator_report,
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

    #[test]
    fn decode_raymarine_locator_beacon() {
        const VIA: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

        // This is a real beacon message from a Raymarine Quantum radar (E704980880217-NewZealand)
        // File "radar transmitting with range changes.pcap.gz"
        // Radar sends from 198.18.6.214 to 224.0.0.1:5800
        // packets of length 36, 56 and 70.
        // Spoke data seems to be on 232.1.243.1:2574
        const DATA1_36: [u8; 36] = [
            0x0, 0x0, 0x0, 0x0, // message type
            0x58, 0x6b, 0xc0, 0xcb, // serial no
            0x29, 0x0, 0x0, 0x0, // submessage 29
            0x3, 0x0, 0x64, 0x0, // ?
            0x6, 0x8, 0x10, 0x0, //
            0x0, 0x1, 0x0, 0xe8, 0x46, 0x14, // 232.0.1.0:5190 multicast send from radar?
            0xe1, 0xc0, // ?
            0xd6, 0x6, 0x12, 0xc6, 0x46, 0x14, // 198.18.6.214:5190 radar address + port
            0x0, 0x0, // ?
        ];
        const DATA1_56: [u8; 56] = [
            0x1, 0x0, 0x0, 0x0, // message type
            0x4d, 0x0, 0x0, 0x0, //
            0x58, 0x6b, 0xc0, 0xcb, // serial no
            0x74, 0x0, 0x0, 0x0, 0xd6, 0x6, 0x12, 0xc6, //
            0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x5f, 0x57, 0x33, 0x0, // "Quantum_W3"
            0x72, 0x0, 0x0, 0x0, //
            0xd, 0x5c, 0xb, 0x1c, 0x80, 0x0, 0x40, 0xc7, 0xd8, 0x1, 0x38, 0x16, 0x39, 0x0, 0x0,
            0x0, 0x0, 0x2, 0x0, 0x0, 0x0,
        ];
        const DATA1_70: [u8; 70] = [
            0x2, 0x0, 0x0, 0x0, // message type
            0x4d, 0x0, 0x0, 0x0, //
            0x58, 0x6b, 0xc0, 0xcb, // serial no
            0x74, 0x0, 0x0, 0x0, //
            0xd6, 0x6, 0x12, 0xc6, //
            0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x5f, 0x57, 0x33, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, // "Quantum_W3" (32 bytes)
            0x10, 0x0, 0x0, 0x0, //
            0x3e, 0x0, 0x0, 0x0, 0x8, 0x0, 0x2, 0x0, 0xd6, 0x6, 0x12, 0xc6, 0x46, 0x14,
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
            0x0, 0x0, 0x0, 0x2, 0x0, 0x0, 0x0,
        ];
        const DATA2_70: [u8; 70] = [
            0x2, 0x0, 0x0, 0x0, // message_type
            0x66, 0x0, 0x0, 0x0, // subtype?
            0x58, 0x6b, 0x80, 0xd6, // link id
            0xf3, 0x0, 0x0, 0x0, 0xa7, 0x27, 0xa8, 0xc0, // ?
            0x51, 0x75, 0x61, 0x6e, 0x74, 0x75, 0x6d, 0x52, 0x61, 0x64, 0x61, 0x72, 0x0, 0x0, 0x0,
            0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0,
            0x0, // "QuantumRadar" (32 bytes)
            0x10, 0x0, 0x0, 0x0, //
            0xee, 0xee, 0x0, 0x0, 0x8, 0x0, 0x2, 0x0, //
            0xdd, 0xc8, 0x1e, 0xa, 0xf, 0xa, // 10.31.30.221:2575
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

        let r = super::process_beacon_report(&DATA1_36, &VIA, false);
        assert!(r.is_ok());
        let r = r.unwrap();
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.model_name(), Some("Quantum".to_string()));
        assert_eq!(r.serial_no, Some("3561010583".to_string()));
        assert_eq!(
            r.send_command_addr,
            SocketAddrV4::new(Ipv4Addr::new(10, 0, 234, 47), 5800)
        );
        assert_eq!(
            r.spoke_data_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 1), 5800)
        );
        assert_eq!(
            r.report_addr,
            SocketAddrV4::new(Ipv4Addr::new(224, 29, 69, 231), 2566)
        );
    }
}
