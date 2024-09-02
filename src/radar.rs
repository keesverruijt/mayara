use enum_primitive_derive::Primitive;
use log::info;
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::locator::LocatorId;
use crate::Cli;

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("Socket operation failed")]
    Io(#[from] std::io::Error),
    #[error("Interface '{0}' is not available")]
    InterfaceNotFound(String),
    #[error("Interface '{0}' has no valid IPv4 address")]
    InterfaceNoV4(String),
    #[error("Cannot detect Ethernet devices")]
    EnumerationFailed,
    #[error("Timeout")]
    Timeout,
    #[error("Shutdown")]
    Shutdown,
}

#[derive(Serialize, Clone, Debug)]
enum PixelType {
    History,
    TargetBorder,
    DopplerApproaching,
    DopplerReceding,
    Normal,
}

#[derive(Clone, Debug)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.a
        )
    }
}

impl Serialize for Color {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Lookup {
    r#type: PixelType,
    color: Color,
}

#[derive(Clone, Debug)]
pub struct Legend {
    pub pixels: Vec<Lookup>,
    pub border: u8,
    pub doppler_approaching: u8,
    pub doppler_receding: u8,
    pub history_start: u8,
}

impl Serialize for Legend {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_map(Some(self.pixels.len()))?;
        for (n, value) in self.pixels.iter().enumerate() {
            let key = n.to_string();
            state.serialize_entry(&key, value)?;
        }
        state.end()
    }
}

#[derive(Clone, Debug)]
pub struct RadarInfo {
    key: String,
    pub id: usize,
    pub locator_id: LocatorId,
    pub brand: String,
    pub model: Option<String>,
    pub serial_no: Option<String>,       // Serial # for this radar
    pub which: Option<String>,           // "A", "B" or None
    pub pixel_values: u8,                // How many values per pixel, 0..220 or so
    pub spokes: u16,                     // How many spokes per rotation
    pub max_spoke_len: u16,              // Fixed for some radars, variable for others
    pub addr: SocketAddrV4,              // The assigned IP address of the radar
    pub nic_addr: Ipv4Addr,              // IPv4 address of NIC via which radar can be reached
    pub spoke_data_addr: SocketAddrV4,   // Where the radar will send data spokes
    pub report_addr: SocketAddrV4,       // Where the radar will send reports
    pub send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
    pub legend: Option<Legend>,          // What pixel values mean

    // Channels
    pub radar_message_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    pub radar_control_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
}

impl RadarInfo {
    pub fn new(
        locator_id: LocatorId,
        brand: &str,
        model: Option<&str>,
        serial_no: Option<&str>,
        which: Option<&str>,
        pixel_values: u8, // How many values per pixel, 0..220 or so
        spokes: usize,
        max_spoke_len: usize,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
    ) -> Self {
        let (radar_message_tx, _radar_message_rx) = tokio::sync::broadcast::channel(32);
        let (radar_control_tx, _radar_control_rx) = tokio::sync::broadcast::channel(32);

        RadarInfo {
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
            id: usize::MAX,
            locator_id,
            brand: brand.to_owned(),
            model: model.map(String::from),
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
            pixel_values,
            spokes: spokes as u16,
            max_spoke_len: max_spoke_len as u16,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            legend: None,
            radar_message_tx,
            radar_control_tx,
            controls: None,
        }
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn set_legend(&mut self, doppler: bool) {
        self.legend = default_legend(doppler, self.pixel_values);
    }

    ///
    ///  forward_output is activated in all starts of radars when cli args.output
    ///  is true:
    ///  
    ///  if args.output {
    ///      subsys.start(SubsystemBuilder::new(data_name, move |s| {
    ///          info.forward_output(s)
    ///      }));
    ///  }
    ///

    pub async fn forward_output(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        use std::io::Write;

        let mut rx = self.radar_message_tx.subscribe();

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Ok(());
                },
                r = rx.recv() => {
                    match r {
                        Ok(r) => {
                            std::io::stdout().write_all(&r).unwrap_or_else(|_| { subsys.request_shutdown(); });
                        },
                        Err(_) => {
                            subsys.request_shutdown();
                        }
                    };
                },
            };
        }
    }
}

impl Display for RadarInfo {
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
    pub info: HashMap<String, RadarInfo>,
    pub args: Cli,
}

impl Radars {
    pub fn new(args: Cli) -> Arc<RwLock<Radars>> {
        Arc::new(RwLock::new(Radars {
            info: HashMap::new(),
            args,
        }))
    }
}
impl Radars {
    // A radar has been found
    pub fn located(new_info: RadarInfo, radars: &Arc<RwLock<Radars>>) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = radars.write().unwrap();
        let count = radars.info.len();

        // For now, drop second radar in replay Mode...
        if radars.args.replay && key.ends_with("-B") {
            return None;
        }
        let entry = radars.info.entry(key).or_insert(new_info);

        if entry.id == usize::MAX {
            entry.id = count;

            info!("Located a new radar: {}", &entry);
            Some(entry.clone())
        } else {
            None
        }
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

pub const BLOB_HISTORY_COLORS: u8 = 32;
const TRANSPARENT: u8 = 0;
const OPAQUE: u8 = 255;

fn default_legend(doppler: bool, pixel_values: u8) -> Legend {
    let mut legend = Legend {
        pixels: Vec::new(),
        history_start: 0,
        border: 0,
        doppler_approaching: 0,
        doppler_receding: 0,
    };

    let mut pixel_values = pixel_values;
    if pixel_values > 255 - 32 - 2 {
        pixel_values = 255 - 32 - 2;
    }

    const WHITE: f32 = 256.;
    let pixels_with_color = pixel_values - 1;
    let start = WHITE / 3.;
    let delta: f32 = WHITE * 2. / (pixels_with_color as f32);
    let one_third = pixels_with_color / 3;
    let two_thirds = one_third * 2;

    // No return is black and transparent
    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color {
            // red starts at 2/3 and peaks at end
            r: 0,
            // green speaks at 2/3
            g: 0,
            // blue peaks at 1/3 and is zero by 2/3
            b: 0,
            a: TRANSPARENT,
        },
    });

    for v in 1..pixel_values {
        legend.pixels.push(Lookup {
            r#type: PixelType::Normal,
            color: Color {
                // red starts at 2/3 and peaks at end
                r: if v >= two_thirds {
                    (start + (v - two_thirds) as f32 * delta) as u8
                } else {
                    0
                },
                // green starts at 1/3 and peaks at 2/3
                g: if v >= one_third && v < two_thirds {
                    (start + (v - one_third) as f32 * delta) as u8
                } else {
                    0
                },
                // blue peaks at 1/3
                b: if v < one_third {
                    (start + v as f32 * (WHITE / pixel_values as f32)) as u8
                } else {
                    0
                },
                a: OPAQUE,
            },
        });
    }

    legend.border = legend.pixels.len() as u8;
    legend.pixels.push(Lookup {
        r#type: PixelType::TargetBorder,
        color: Color {
            r: 200,
            g: 200,
            b: 200,
            a: OPAQUE,
        },
    });

    if doppler {
        legend.doppler_approaching = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerApproaching,
            color: Color {
                r: 0,
                g: 200,
                b: 200,
                a: OPAQUE,
            },
        });
        legend.doppler_receding = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerReceding,
            color: Color {
                r: 0x90,
                g: 0xd0,
                b: 0xf0,
                a: OPAQUE,
            },
        });
    }

    legend.history_start = legend.pixels.len() as u8;
    const START_DENSITY: u8 = 255; // Target trail starts as white
    const END_DENSITY: u8 = 63; // Ends as gray
    const DELTA_INTENSITY: u8 = (START_DENSITY - END_DENSITY) / BLOB_HISTORY_COLORS;
    let mut density = START_DENSITY;
    for _history in 0..BLOB_HISTORY_COLORS {
        let color = Color {
            r: density,
            g: density,
            b: density,
            a: OPAQUE,
        };
        density -= DELTA_INTENSITY;
        legend.pixels.push(Lookup {
            r#type: PixelType::History,
            color,
        });
    }

    legend
}

#[cfg(test)]
mod tests {

    use super::default_legend;

    #[test]
    fn legend() {
        let legend = default_legend(true, 16);
        let json = serde_json::to_string_pretty(&legend).unwrap();
        println!("{}", json);
    }
}
