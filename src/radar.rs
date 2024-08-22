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

use crate::locator::LocatorId;

#[derive(Serialize, Clone, Debug)]
enum PixelType {
    History,
    TargetBorder,
    DopplerApproaching,
    DopplerReceding,
    Normal,
}

// The Target Trails code is the same on all radars, and all spoke
// pixel values contain [0..32> as history values.
pub const BLOB_HISTORY_COLORS: u8 = 32;
pub const BLOB_TARGET_BORDER: u8 = 32;
pub const BLOB_DOPPLER_APPROACHING: u8 = 33;
pub const BLOB_DOPPLER_RECEDING: u8 = 34;
pub const BLOB_NORMAL_START: u8 = 35;

pub fn map_pixel_to_type(p: u8) -> PixelType {
    match p {
        0..BLOB_HISTORY_COLORS => PixelType::History,
        BLOB_TARGET_BORDER => PixelType::TargetBorder,
        BLOB_DOPPLER_APPROACHING => PixelType::DopplerApproaching,
        BLOB_DOPPLER_RECEDING => PixelType::DopplerReceding,
        _ => PixelType::Normal,
    }
}

#[derive(Clone, Debug)]
struct Colour {
    r: u8,
    g: u8,
    b: u8,
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

impl Serialize for Colour {
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
    colour: Colour,
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
}

impl RadarInfo {
    pub fn new(
        locator_id: LocatorId,
        brand: &str,
        model: Option<&str>,
        serial_no: Option<&str>,
        which: Option<&str>,
        pixel_values: u8, // How many values per pixel, 0..220 or so
        spokes: u16,
        max_spoke_len: u16,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
    ) -> Self {
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
            id: 0,
            locator_id,
            brand: brand.to_owned(),
            model: model.map(String::from),
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
            pixel_values,
            spokes,
            max_spoke_len,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            legend: None,
        }
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn set_legend(&mut self, doppler: bool) {
        self.legend = Some(default_legend(doppler, self.pixel_values));
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
pub fn located(new_info: RadarInfo, radars: &Arc<RwLock<Radars>>) -> Option<RadarInfo> {
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

    // No return is black
    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        colour: Colour {
            // red starts at 2/3 and peaks at end
            r: 0,
            // green speaks at 2/3
            g: 0,
            // blue peaks at 1/3 and is zero by 2/3
            b: 0,
        },
    });

    for v in 1..pixel_values {
        legend.pixels.push(Lookup {
            r#type: PixelType::Normal,
            colour: Colour {
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
            },
        });
    }

    legend.border = legend.pixels.len() as u8;
    legend.pixels.push(Lookup {
        r#type: PixelType::TargetBorder,
        colour: Colour {
            r: 200,
            g: 200,
            b: 200,
        },
    });

    if doppler {
        legend.doppler_approaching = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerApproaching,
            colour: Colour {
                r: 0,
                g: 200,
                b: 200,
            },
        });
        legend.doppler_receding = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerReceding,
            colour: Colour {
                r: 0x90,
                g: 0xd0,
                b: 0xf0,
            },
        });
    }

    legend.history_start = legend.pixels.len() as u8;
    const START_DENSITY: u8 = 255; // Target trail starts as white
    const END_DENSITY: u8 = 63; // Ends as gray
    const DELTA_INTENSITY: u8 = (START_DENSITY - END_DENSITY) / BLOB_HISTORY_COLORS;
    let mut density = START_DENSITY;
    for _history in 0..BLOB_HISTORY_COLORS {
        let colour = Colour {
            r: density,
            g: density,
            b: density,
        };
        density -= DELTA_INTENSITY;
        legend.pixels.push(Lookup {
            r#type: PixelType::History,
            colour,
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
