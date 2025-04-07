use async_trait::async_trait;
use bincode::deserialize;
use log::log_enabled;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator, RadarLocatorState};
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{c_string, PrintableSlice};
use crate::{Brand, GLOBAL_ARGS};

mod command;
mod data;
mod report;
mod settings;

const FURUNO_SPOKES: usize = 8192;

// Maximum supported Length of a spoke in pixels.
const FURUNO_SPOKE_LEN: usize = 1024;

const FURUNO_BASE_PORT: u16 = 10000;
const FURUNO_BEACON_PORT: u16 = FURUNO_BASE_PORT + 10;
const FURUNO_DATA_PORT: u16 = FURUNO_BASE_PORT + 24;
const FURUNO_COMMAND_PORT: u16 = FURUNO_BASE_PORT + 100;

const FURUNO_BEACON_ADDRESS: SocketAddr = SocketAddr::new(
    IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)),
    FURUNO_BEACON_PORT,
);

const FURUNO_ANNOUNCE_MAYARA_PACKET: [u8; 32] = [
    0x1, 0x0, 0x0, 0x1, 0x0, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x18, 0x1, 0x0, 0x0, 0x0, b'M', b'A',
    b'Y', b'A', b'R', b'A', 0x0, 0x0, 0x1, 0x1, 0x0, 0x2, 0x0, 0x1, 0x0, 0x12,
];

enum CommandMode {
    Set,
    Request,
    New,
    X,
    E,
    O,
}

impl CommandMode {
    fn to_char(&self) -> char {
        match self {
            CommandMode::Set => 'S',
            CommandMode::Request => 'R',
            CommandMode::New => 'N',
            CommandMode::X => 'X',
            CommandMode::E => 'E',
            CommandMode::O => 'O',
            // Add more cases as needed
        }
    }
}

impl From<u8> for CommandMode {
    fn from(item: u8) -> Self {
        match item {
            b'S' => CommandMode::Set,
            b'R' => CommandMode::Request,
            b'N' => CommandMode::New,
            b'X' => CommandMode::X,
            b'E' => CommandMode::E,
            b'O' => CommandMode::O,
            _ => CommandMode::New,
        }
    }
}

// From MaxSea.Radar.BusinessObjects.RadarRanges
static FURUNO_RADAR_RANGES: [i32; 22] = [
    115,  // 1/16nm
    231,  // 1/8nm
    463,  // 1/4nm
    926,  // 1/2nm
    1389, // 3/4nm
    1852,
    2778, // 1,5nm
    1852 * 2,
    1852 * 3,
    1852 * 4,
    1852 * 6,
    1852 * 8,
    1852 * 12,
    1852 * 16,
    1852 * 24,
    1852 * 32,
    1852 * 36,
    1852 * 48,
    1852 * 64,
    1852 * 72,
    1852 * 96,
    1852 * 120,
];

fn found(info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) -> bool {
    info.controls
        .set_string(&crate::settings::ControlType::UserName, info.key())
        .unwrap();

    if let Some(mut info) = radars.located(info) {
        // It's new, start the RadarProcessor thread

        // Load the model name afresh, it may have been modified from persisted data
        /* let model = match info.model_name() {
            Some(s) => Model::new(&s),
            None => Model::Unknown,
        };
        if model != Model::Unknown {
            let info2 = info.clone();
            info.controls.update_when_model_known(model, &info2);
            info.set_legend(model == Model::HALO);
            radars.update(&info);
        } */

        // Furuno radars use a single TCP/IP connection to send commands and
        // receive status reports, so report_addr and send_command_addr are identical.
        // Only one of these would be enough for Furuno.
        let port: u16 = match login_to_radar(info.addr) {
            Err(e) => {
                log::error!("{}: Unable to connect for login: {}", info.key(), e);
                radars.remove(&info.key());
                return false;
            }
            Ok(p) => p,
        };
        if port != info.send_command_addr.port() {
            info.send_command_addr.set_port(port);
            info.report_addr.set_port(port);
            radars.update(&info);
        }

        // Clone everything moved into future twice or more
        let data_name = info.key() + " data";
        let report_name = info.key() + " reports";

        if GLOBAL_ARGS.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        let data_receiver = data::FurunoDataReceiver::new(info.clone());
        subsys.start(SubsystemBuilder::new(
            data_name,
            move |s: SubsystemHandle| data_receiver.run(s),
        ));

        if !GLOBAL_ARGS.replay {
            let report_receiver = report::FurunoReportReceiver::new(info);
            subsys.start(SubsystemBuilder::new(report_name, |s| {
                report_receiver.run(s)
            }));
        }

        return true;
    }
    return false;
}

// [01, 00, 00, 01, 00, 00, 00, 00, 00, 01, 00, 18, 01, 00, 00, 00, 52, 44, 30, 30, 33, 32, 31, 32, 01, 01, 00, 02, 00, 01, 00, 12] len 32
// [ .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   .   R   D   0   0   3   2   1   2   .   .   .   .   .   .   .   .]
//                                               ^_type?             ^_name, always 8 long?

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct FurunoRadarReport {
    _filler1: [u8; 11],
    device_type: u8,
    _filler2: [u8; 4],
    name: [u8; 8],
    _filler3: [u8; 8],
}

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct FurunoRadarModelReport {
    _filler1: [u8; 24],
    model: [u8; 32],
    _firmware_versions: [u8; 32],
    _firmware_version: [u8; 32],
    serial_no: [u8; 32],
    _filler2: [u8; 18],
}

const LOGIN_TIMEOUT: Duration = Duration::from_millis(500);

fn login_to_radar(radar_addr: SocketAddrV4) -> Result<u16, io::Error> {
    if GLOBAL_ARGS.replay {
        log::warn!(
            "Replay mode, not logging in to radar and assuming data port {}",
            FURUNO_DATA_PORT
        );
        return Ok(FURUNO_DATA_PORT);
    }

    let mut stream =
        std::net::TcpStream::connect_timeout(&std::net::SocketAddr::V4(radar_addr), LOGIN_TIMEOUT)?;

    // fnet.dll function "login_via_copyright"
    // From the 13th byte the message is:
    // "COPYRIGHT (C) 2001 FURUNO ELECTRIC CO.,LTD. "
    const LOGIN_MESSAGE: [u8; 56] = [
        //                                              v- this byte is the only variable one
        0x8, 0x1, 0x0, 0x38, 0x1, 0x0, 0x0, 0x0, 0x0, 0x1, 0x0, 0x0, 0x43, 0x4f, 0x50, 0x59, 0x52,
        0x49, 0x47, 0x48, 0x54, 0x20, 0x28, 0x43, 0x29, 0x20, 0x32, 0x30, 0x30, 0x31, 0x20, 0x46,
        0x55, 0x52, 0x55, 0x4e, 0x4f, 0x20, 0x45, 0x4c, 0x45, 0x43, 0x54, 0x52, 0x49, 0x43, 0x20,
        0x43, 0x4f, 0x2e, 0x2c, 0x4c, 0x54, 0x44, 0x2e, 0x20,
    ];
    const EXPECTED_HEADER: [u8; 8] = [0x9, 0x1, 0x0, 0xc, 0x1, 0x0, 0x0, 0x0];

    stream.set_write_timeout(Some(LOGIN_TIMEOUT))?;
    stream.set_read_timeout(Some(LOGIN_TIMEOUT))?;

    stream.write_all(&LOGIN_MESSAGE)?;

    let mut buf: [u8; 8] = [0; 8];
    stream.read_exact(&mut buf)?;

    if buf != EXPECTED_HEADER {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("Unexpected reply {:?}", buf),
        ));
    }
    stream.read_exact(&mut buf[0..4])?;

    let port = FURUNO_BASE_PORT + ((buf[0] as u16) << 8) + buf[1] as u16;
    log::debug!(
        "Furuno radar logged in; using port {} for report/command data",
        port
    );
    Ok(port)
}

#[derive(Clone)]
struct FurunoLocatorState {
    radars: HashMap<SocketAddrV4, String>,
}

impl RadarLocatorState for FurunoLocatorState {
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
        Box::new(Clone::clone(self))
    }
}

impl FurunoLocatorState {
    fn process_locator_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        via: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> io::Result<()> {
        if report.len() < 2 {
            return Ok(());
        }

        if log_enabled!(log::Level::Debug) {
            log::debug!(
                "{}: Furuno report: {:02X?} len {}",
                from,
                report,
                report.len()
            );
            log::debug!("{}: printable:     {}", from, PrintableSlice::new(report));
        }

        if report.len() == 32 && report[16] == b'R' && report[17] == b'D' {
            self.process_beacon_report(report, from, via, radars, subsys)
        } else if report.len() == 170 {
            self.process_beacon_model_report(report, from, via, radars)
        } else {
            Ok(())
        }
    }

    fn process_beacon_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        match deserialize::<FurunoRadarReport>(report) {
            Ok(data) => {
                if let Some(name) = c_string(&data.name) {
                    if data.device_type != 0x18 {
                        log::warn!(
                            "Radar info packet uses device type {} instead of 24",
                            data.device_type
                        );
                    }
                    let radar_addr: SocketAddrV4 = from.clone();

                    // DRS: spoke data all on a well-known address
                    let spoke_data_addr: SocketAddrV4 =
                        SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), FURUNO_DATA_PORT);

                    let report_addr: SocketAddrV4 =
                        SocketAddrV4::new(*from.ip(), FURUNO_COMMAND_PORT);
                    let send_command_addr: SocketAddrV4 = report_addr.clone();
                    let location_info: RadarInfo = RadarInfo::new(
                        LocatorId::Furuno,
                        Brand::Furuno,
                        None,
                        Some(name),
                        64,
                        FURUNO_SPOKES,
                        FURUNO_SPOKE_LEN,
                        radar_addr,
                        nic_addr.clone(),
                        spoke_data_addr,
                        report_addr,
                        send_command_addr,
                        settings::new(),
                        true,
                    );
                    let key = location_info.key();
                    if found(location_info, radars, subsys) {
                        self.radars.insert(from.clone(), key);
                    }
                }
            }
            Err(e) => {
                log::error!(
                    "{} via {}: Failed to decode Furuno radar report: {}",
                    from,
                    nic_addr,
                    e
                );
            }
        }

        Ok(())
    }

    fn process_beacon_model_report(
        &mut self,
        report: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
    ) -> Result<(), io::Error> {
        let radar_addr: SocketAddrV4 = from.clone();
        // Is this known as a Furuno radar?
        if let Some(key) = self.radars.get(&radar_addr) {
            match deserialize::<FurunoRadarModelReport>(report) {
                Ok(data) => {
                    let model = c_string(&data.model);
                    let serial_no = c_string(&data.serial_no);

                    if let (Some(model), Some(serial_no)) = (model, serial_no) {
                        radars.update_serial_no(key, serial_no.to_string());
                        if let Ok(radar_info) = radars.find_radar_info(key) {
                            let mut controls = radar_info.controls.clone();
                            settings::update_when_model_known(&mut controls, &radar_info, model);
                        }
                    }
                }
                Err(e) => {
                    log::error!(
                        "{} via {}: Failed to decode Furuno radar report: {}",
                        from,
                        nic_addr,
                        e
                    );
                }
            }
        }

        Ok(())
    }
}

struct FurunoLocator {}

#[async_trait]
impl RadarLocator for FurunoLocator {
    fn set_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses.iter().any(|i| i.id == LocatorId::Furuno) {
            addresses.push(LocatorAddress::new(
                LocatorId::Furuno,
                &FURUNO_BEACON_ADDRESS,
                Brand::Furuno,
                Some(&FURUNO_ANNOUNCE_MAYARA_PACKET),
                Box::new(FurunoLocatorState {
                    radars: HashMap::new(),
                }),
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = FurunoLocator {};
    Box::new(locator)
}
