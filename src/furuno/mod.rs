use async_trait::async_trait;
use bincode::deserialize;
use log::{log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorId, RadarListenAddress, RadarLocator};
use crate::radar::{DopplerMode, Legend, RadarInfo, SharedRadars};
use crate::util::{c_string, PrintableSlice};

mod data;
mod settings;

const FURUNO_SPOKES: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
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
        let args = radars.cli_args();

        if args.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        let data_receiver = data::FurunoDataReceiver::new(info, rx_data, args.replay);
        subsys.start(SubsystemBuilder::new(
            data_name,
            move |s: SubsystemHandle| data_receiver.run(s),
        ));
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

    if log_enabled!(log::Level::Info) {
        log::info!(
            "{}: Furuno report: {:02X?} len {}",
            from,
            report,
            report.len()
        );
        log::info!("{}: printable:     {}", from, PrintableSlice::new(report));
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
    via: &Ipv4Addr,
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

                let radar_data: SocketAddrV4 =
                    SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), 10024);
                let radar_report: SocketAddrV4 = radar_addr.into();
                let radar_send: SocketAddrV4 = radar_addr.into();
                let location_info: RadarInfo = RadarInfo::new(
                    LocatorId::Furuno,
                    "Furuno",
                    None,
                    Some(name),
                    16,
                    FURUNO_SPOKES,
                    FURUNO_SPOKE_LEN,
                    radar_addr.into(),
                    via.clone(),
                    radar_data.into(),
                    radar_report.into(),
                    radar_send.into(),
                    settings::new(radars.cli_args().replay),
                );
                found(location_info, radars, subsys);
            }
        }
        Err(e) => {
            log::error!(
                "{} via {}: Failed to decode Furuno radar report: {}",
                from,
                via,
                e
            );
        }
    }

    Ok(())
}

struct FurunoLocator {}

#[async_trait]
impl RadarLocator for FurunoLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>) {
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::Furuno && i.brand == "Furuno Beacon")
        {
            addresses.push(RadarListenAddress::new(
                LocatorId::Furuno,
                &FURUNO_BEACON_ADDRESS,
                "Furuno Beacon",
                None,
                &process_locator_report,
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = FurunoLocator {};
    Box::new(locator)
}
