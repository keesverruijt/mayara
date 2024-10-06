use async_trait::async_trait;
use log::{log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorId, RadarListenAddress, RadarLocator};
use crate::radar::{DopplerMode, Legend, RadarInfo, SharedRadars};
use crate::util::PrintableSlice;

mod settings;

const FURUNO_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255)), 10010);

fn found(mut info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    info.set_string(&crate::settings::ControlType::UserName, info.key())
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

        // let (tx_data, rx_data) = mpsc::channel(10);

        // Clone everything moved into future twice or more
        let data_name = info.key() + " data";
        let report_name = info.key() + " reports";
        let info_clone = info.clone();
        let args = radars.cli_args();

        if args.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        /*
        let data_receiver = data::FurunoDataReceiver::new(info, rx_data, args.replay);
        let report_receiver =
            report::FurunoReportReceiver::new(info_clone, radars.clone(), model, tx_data);

        subsys.start(SubsystemBuilder::new(
            data_name,
            move |s: SubsystemHandle| data_receiver.run(s),
        ));
        subsys.start(SubsystemBuilder::new(report_name, |s| {
            report_receiver.run(s)
        }));
        */
    }
}

fn process_locator_report(
    report: &[u8],
    from: &SocketAddr,
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

    if report[0] == 0x1 && report[1] == 0xB2 {
        // Common Furuno message

        return process_beacon_report(report, from, via, radars, subsys);
    }
    Ok(())
}

fn process_beacon_report(
    report: &[u8],
    from: &SocketAddr,
    via: &Ipv4Addr,
    radars: &SharedRadars,
    subsys: &SubsystemHandle,
) -> Result<(), io::Error> {
    /*
    match deserialize::<FurunoBeaconSingle>(report) {
        Ok(data) => {
            if let Some(serial_no) = c_string(&data.header.serial_no) {
                let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                let radar_data: SocketAddrV4 = data.a.data.into();
                let radar_report: SocketAddrV4 = data.a.report.into();
                let radar_send: SocketAddrV4 = data.a.send.into();
                let location_info: RadarInfo = RadarInfo::new(
                    LocatorId::Gen3Plus,
                    "Furuno",
                    Some(serial_no),
                    None,
                    16,
                    FURUNO_SPOKES,
                    FURUNO_SPOKE_LEN,
                    radar_addr.into(),
                    via.clone(),
                    radar_data.into(),
                    radar_report.into(),
                    radar_send.into(),
                    FurunoControls::new(None),
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
    } */

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
