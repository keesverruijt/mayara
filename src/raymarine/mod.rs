use async_trait::async_trait;
use bincode::deserialize;
use log::{log_enabled, trace};
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::{fmt, io};
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

use crate::locator::{LocatorId, RadarListenAddress, RadarLocator};
use crate::radar::{RadarInfo, SharedRadars};
use crate::util::PrintableSlice;

mod settings;

const RAYMARINE_SPOKES: usize = 2048;

// Length of a spoke in pixels. Every pixel is 4 bits (one nibble.)
const RAYMARINE_SPOKE_LEN: usize = 1024;

const RAYMARINE_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(224, 0, 0, 1)), 5800);
// 224/8 is not routable, so for WiFi they added a second address
const RAYMARINE_ALT_BEACON_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(232, 1, 1, 1)), 5800);

fn found(mut info: RadarInfo, radars: &SharedRadars, subsys: &SubsystemHandle) {
    info.set_string(&crate::settings::ControlType::UserName, info.key())
        .unwrap();

    if let Some(info) = radars.located(info) {
        // It's new, start the RadarProcessor thread

        //        let (_tx_data, rx_data) = mpsc::channel(10);

        // Clone everything moved into future twice or more
        let data_name = info.key() + " data";
        let args = radars.cli_args();

        if args.output {
            let info_clone2 = info.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }

        /*
        let data_receiver = data::RaymarineDataReceiver::new(info, rx_data, args.replay);
        subsys.start(SubsystemBuilder::new(
            data_name,
            move |s: SubsystemHandle| data_receiver.run(s),
        ));
        */
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
            "{}: Raymarine report: {:02X?} len {}",
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

#[derive(Deserialize, Debug, Copy, Clone)]
#[repr(packed)]
struct RaymarineRadarReport {
    field1: u32,     // 0
    field2: u32,     // 4
    model_id: u8,    // 0x28 byte 8
    field3: u8,      // byte 9
    field4: u16,     // byte 10
    field5: u32,     // 12
    field6: u32,     // 16
    data_ip: u32,    // 20
    data_port: u32,  // 24
    radar_ip: u32,   // 28
    radar_port: u32, // 32
}

fn process_beacon_report(
    report: &[u8],
    from: &SocketAddrV4,
    via: &Ipv4Addr,
    radars: &SharedRadars,
    subsys: &SubsystemHandle,
) -> Result<(), io::Error> {
    match deserialize::<RaymarineRadarReport>(report) {
        Ok(data) => {
            let name = match data.model_id {
                0x01 => "E120",
                0x28 => "Quantum",
                _ => {
                    log::error!("Unhandled Raymarine radar type 0x{:02x}", data.model_id);
                    return Ok(());
                }
            };

            let radar_addr: SocketAddrV4 = from.clone();

            let radar_data: SocketAddrV4 =
                SocketAddrV4::new(Ipv4Addr::from_bits(data.data_ip), data.data_port as u16);
            let radar_report: SocketAddrV4 =
                SocketAddrV4::new(Ipv4Addr::from_bits(data.data_ip), data.data_port as u16);
            let radar_send: SocketAddrV4 =
                SocketAddrV4::new(Ipv4Addr::from_bits(data.radar_ip), data.radar_port as u16);
            let location_info: RadarInfo = RadarInfo::new(
                LocatorId::Raymarine,
                "Raymarine",
                None,
                Some(name),
                16,
                RAYMARINE_SPOKES,
                RAYMARINE_SPOKE_LEN,
                radar_addr.into(),
                via.clone(),
                radar_data.into(),
                radar_report.into(),
                radar_send.into(),
                settings::new(),
            );
            found(location_info, radars, subsys);
        }
        Err(e) => {
            log::error!(
                "{} via {}: Failed to decode Raymarine radar report: {}",
                from,
                via,
                e
            );
        }
    }

    Ok(())
}

struct RaymarineLocator {}

#[async_trait]
impl RadarLocator for RaymarineLocator {
    fn update_listen_addresses(&self, addresses: &mut Vec<RadarListenAddress>) {
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::Raymarine && i.brand == "Raymarine Beacon")
        {
            addresses.push(RadarListenAddress::new(
                LocatorId::Raymarine,
                &RAYMARINE_BEACON_ADDRESS,
                "Raymarine Beacon",
                None,
                &process_locator_report,
            ));
        }
        if !addresses
            .iter()
            .any(|i| i.id == LocatorId::Raymarine && i.brand == "Raymarine Alternate Beacon")
        {
            addresses.push(RadarListenAddress::new(
                LocatorId::Raymarine,
                &RAYMARINE_ALT_BEACON_ADDRESS,
                "Raymarine Alternate Beacon",
                None,
                &process_locator_report,
            ));
        }
    }
}

pub fn create_locator() -> Box<dyn RadarLocator + Send> {
    let locator = RaymarineLocator {};
    Box::new(locator)
}
