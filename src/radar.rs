use async_trait::async_trait;
use log::info;
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    io,
    net::SocketAddr,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct RadarLocationInfo {
    id: usize,
    pub brand: String,
    pub model: Option<String>,
    pub serial_no: Option<String>,     // Serial # for this radar
    pub which: Option<String>,         // "A", "B" or None
    pub addr: SocketAddr,              // The assigned IP address of the radar
    pub spoke_data_addr: SocketAddr,   // Where the radar will send data spokes
    pub report_addr: SocketAddr,       // Where the radar will send reports
    pub send_command_addr: SocketAddr, // Where displays will send commands to the radar
}

impl RadarLocationInfo {
    pub fn new(
        brand: &str,
        model: Option<&str>,
        serial_no: Option<&str>,
        which: Option<&str>,
        addr: SocketAddr,
        data: SocketAddr,
        report: SocketAddr,
        send: SocketAddr,
    ) -> Self {
        RadarLocationInfo {
            id: 0,
            brand: brand.to_owned(),
            model: model.map(String::from),
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
            addr: addr,
            spoke_data_addr: data,
            report_addr: report,
            send_command_addr: send,
        }
    }
}

impl Display for RadarLocationInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Radar {}", &self.brand)?;
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
            " at {} data {} report {} send {}",
            &self.addr.ip(),
            &self.spoke_data_addr,
            &self.report_addr,
            &self.send_command_addr
        )
    }
}

pub struct Radars {
    info: HashMap<String, RadarLocationInfo>,
}

impl Radars {
    pub fn new() -> Arc<RwLock<Radars>> {
        Arc::new(RwLock::new(Radars {
            info: HashMap::new(),
        }))
    }
}

pub struct Statistics {
    broken_packets: usize,
}

#[async_trait]
pub trait RadarProcessor {
    async fn process(&mut self, info: RadarLocationInfo) -> io::Result<()>;
}

// A radar has been found
pub fn located(
    new_info: RadarLocationInfo,
    radars: &Arc<RwLock<Radars>>,
) -> Option<RadarLocationInfo> {
    let key = get_key(&new_info);
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

fn get_key(info: &RadarLocationInfo) -> String {
    let mut key = info.brand.clone();

    if let Some(serial_no) = &info.serial_no {
        key.push_str("-");
        key.push_str(serial_no);
    } else {
        write!(key, "-{}", &info.addr).unwrap();
    }

    if let Some(which) = &info.which {
        key.push_str("-");
        key.push_str(which);
    }

    key
}
