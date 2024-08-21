use enum_primitive_derive::Primitive;
use log::info;
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
};

use crate::locator::LocatorId;

#[derive(Clone, Debug)]
pub struct RadarLocationInfo {
    key: String,
    pub id: usize,
    pub locator_id: LocatorId,
    pub brand: String,
    pub model: Option<String>,
    pub serial_no: Option<String>, // Serial # for this radar
    pub which: Option<String>,     // "A", "B" or None
    pub spokes: u16,
    pub max_spoke_len: u16,
    pub addr: SocketAddrV4,            // The assigned IP address of the radar
    pub nic_addr: Ipv4Addr,            // IPv4 address of NIC via which radar can be reached
    pub spoke_data_addr: SocketAddrV4, // Where the radar will send data spokes
    pub report_addr: SocketAddrV4,     // Where the radar will send reports
    pub send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
}

impl RadarLocationInfo {
    pub fn new(
        locator_id: LocatorId,
        brand: &str,
        model: Option<&str>,
        serial_no: Option<&str>,
        which: Option<&str>,
        spokes: u16,
        max_spoke_len: u16,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
    ) -> Self {
        RadarLocationInfo {
            key: {
                let mut key = brand.to_string();

                if let Some(serial_no) = serial_no {
                    key.push_str("-");
                    key.push_str(serial_no);
                } else {
                    write!(key, "-{}", &addr).unwrap();
                }

                if let Some(which) = which {
                    key.push_str("-");
                    key.push_str(which);
                }
                key
            },
            id: 0,
            locator_id,
            brand: brand.to_owned(),
            model: model.map(String::from),
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
            spokes,
            max_spoke_len,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
        }
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }
}

impl Display for RadarLocationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Radar {} locator {} brand {}",
            &self.id,
            &self.locator_id.as_str(),
            &self.brand
        )?;
        if let Some(model) = &self.model {
            write!(f, " {}", model)?;
        }
        if let Some(which) = &self.which {
            write!(f, " {}", which)?;
        }
        if let Some(serial_no) = &self.serial_no {
            write!(f, " [{}]", serial_no)?;
        }
        write!(
            f,
            " at {} via {} data {} report {} send {}",
            &self.addr.ip(),
            &self.nic_addr,
            &self.spoke_data_addr,
            &self.report_addr,
            &self.send_command_addr
        )
    }
}

#[derive(Clone, Debug)]
pub struct Radars {
    pub info: HashMap<String, RadarLocationInfo>,
}

impl Radars {
    pub fn new() -> Arc<RwLock<Radars>> {
        Arc::new(RwLock::new(Radars {
            info: HashMap::new(),
        }))
    }
}

pub struct Statistics {
    pub broken_packets: usize,
}

// The actual values are not arbitrary: these are the exact values as reported
// by HALO radars, simplifying the navico::report code.
#[derive(Copy, Clone, Debug, Primitive)]
pub enum DopplerMode {
    None = 0,
    Both = 1,
    Approaching = 2,
}

impl fmt::Display for DopplerMode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

// A radar has been found
pub fn located(
    new_info: RadarLocationInfo,
    radars: &Arc<RwLock<Radars>>,
) -> Option<RadarLocationInfo> {
    let key = new_info.key.to_owned();
    let mut radars = radars.write().unwrap();
    let count = radars.info.len();
    let entry = radars.info.entry(key).or_insert(new_info);

    if entry.id == 0 {
        entry.id = count + 1;

        info!("Located a new radar: {}", &entry);
        Some(entry.clone())
    } else {
        None
    }
}
