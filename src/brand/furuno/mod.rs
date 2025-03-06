use async_trait::async_trait;
use bincode::deserialize;
use log::log_enabled;
use serde::Deserialize;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorAddress, LocatorId, RadarLocator, RadarLocatorState};
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::{c_string, PrintableSlice};

mod data;
mod report;
mod settings;

const FURUNO_SPOKES: usize = 8192;

// Maximum supported Length of a spoke in pixels.
const FURUNO_SPOKE_LEN: usize = 1024;

const FURUNO_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)), 10010);

fn found(mut info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    info.set_string(&crate::settings::ControlType::UserName, info.key())
        .unwrap();

    if let Some(info) = radars.located(info) {
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

        // let (tx_data, rx_data) = mpsc::channel(10);
        let (_tx_data, rx_data) = mpsc::channel(10);

        // Clone everything moved into future twice or more
        let data_name = info.key() + " data";
        let report_name = info.key() + " reports";
        let args = radars.cli_args();

        if args.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        let data_receiver = data::FurunoDataReceiver::new(info.clone(), rx_data, args);
        subsys.start(SubsystemBuilder::new(
            data_name,
            move |s: SubsystemHandle| data_receiver.run(s),
        ));

        let report_receiver = report::FurunoReportReceiver::new(info, radars.clone());
        subsys.start(SubsystemBuilder::new(report_name, |s| {
            report_receiver.run(s)
        }));
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
        return process_beacon_report(report, from, via, radars, subsys);
    }
    Ok(())
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

fn process_beacon_report(
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
                    SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), 10024);
                let report_addr: SocketAddrV4 =
                    SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), 10094);
                let send_command_addr: SocketAddrV4 = radar_addr.clone();
                let location_info: RadarInfo = RadarInfo::new(
                    LocatorId::Furuno,
                    "Furuno",
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
                    settings::new(radars.cli_args().replay),
                );
                found(location_info, radars, subsys);
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

#[derive(Clone, Copy)]
struct FurunoLocatorState {}

impl RadarLocatorState for FurunoLocatorState {
    fn process(
        &mut self,
        message: &[u8],
        from: &SocketAddrV4,
        nic_addr: &Ipv4Addr,
        radars: &SharedRadars,
        subsys: &SubsystemHandle,
    ) -> Result<(), io::Error> {
        process_locator_report(message, from, nic_addr, radars, subsys)
    }

    fn clone(&self) -> Box<dyn RadarLocatorState> {
        Box::new(FurunoLocatorState {}) // Navico is stateless
    }
}

struct FurunoLocator {}

#[async_trait]
impl RadarLocator for FurunoLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<LocatorAddress>) {
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::Furuno && i.brand == "Furuno Beacon")
        {
            addresses.push(LocatorAddress::new(
                LocatorId::Furuno,
                &FURUNO_BEACON_ADDRESS,
                "Furuno Beacon",
                None,
                Box::new(FurunoLocatorState {}),
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = FurunoLocator {};
    Box::new(locator)
}
