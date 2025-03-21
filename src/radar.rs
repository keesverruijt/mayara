use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_primitive_derive::Primitive;
use log::info;
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio_graceful_shutdown::SubsystemHandle;

pub(crate) mod target;
pub(crate) mod trail;

use crate::config::Persistence;
use crate::locator::LocatorId;
use crate::settings::{ControlError, ControlType, ControlUpdate, ControlValue, SharedControls};
use crate::{Brand, GLOBAL_ARGS};

// A "native to radar" bearing, usually [0..2048] or [0..4096] or [0..8192]
pub(crate) type SpokeBearing = u16;

#[derive(Error, Debug)]
pub enum RadarError {
    #[error("I/O operation failed")]
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
    #[error("{0}")]
    ControlError(#[from] ControlError),
    #[error("Cannot set value for control '{0}'")]
    CannotSetControlType(ControlType),
    #[error("Missing value for control '{0}'")]
    MissingValue(ControlType),
    #[error("No such radar with key '{0}'")]
    NoSuchRadar(String),
    #[error("Cannot parse JSON '{0}'")]
    ParseJson(String),
    #[error("IP address changed")]
    IPAddressChanged,
    #[cfg(windows)]
    #[error("OS error: {0}")]
    OSError(String),
}

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for RadarError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self).into_response()
    }
}

//
// This order of pixeltypes is also how they are stored in the legend.
//
#[derive(Serialize, Clone, Debug)]
enum PixelType {
    Normal,
    TargetBorder,
    DopplerApproaching,
    DopplerReceding,
    History,
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
    pub strong_return: u8,
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
pub struct RangeDetection {
    pub complete: bool,
    pub saved_range: i32,
    pub commanded_range: i32,
    pub min_range: i32,
    pub max_range: i32,
    pub ranges: Vec<i32>,
}

impl RangeDetection {
    pub fn new(min_range: i32, max_range: i32) -> Self {
        RangeDetection {
            complete: false,
            saved_range: 0,
            commanded_range: 0,
            min_range,
            max_range,
            ranges: Vec::new(),
        }
    }

    pub fn restore(ranges: &Vec<i32>) -> Self {
        RangeDetection {
            complete: true,
            saved_range: 0,
            commanded_range: 0,
            min_range: *ranges.first().unwrap_or(&0),
            max_range: *ranges.last().unwrap_or(&0),
            ranges: ranges.clone(),
        }
    }
}

/// A geographic position expressed in degrees latitude and longitude.
/// Latitude is positive in the northern hemisphere, negative in the southern.
/// Longitude is positive in the eastern hemisphere, negative in the western.
/// The range for latitude is -90 to 90, and for longitude is -180 to 180.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct GeoPosition {
    lat: f64,
    lon: f64,
}

impl GeoPosition {
    pub(crate) fn new(lat: f64, lon: f64) -> Self {
        GeoPosition { lat, lon }
    }
}

impl fmt::Display for GeoPosition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.lat, self.lon)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RadarInfo {
    key: String,
    pub id: usize,
    pub locator_id: LocatorId,
    pub brand: Brand,
    pub serial_no: Option<String>,       // Serial # for this radar
    pub which: Option<String>,           // "A", "B" or None
    pub pixel_values: u8,                // How many values per pixel, 0..220 or so
    pub spokes: u16,                     // How many spokes per rotation
    pub max_spoke_len: u16,              // Fixed for some radars, variable for others
    pub addr: SocketAddrV4,              // The IP address of the radar
    pub nic_addr: Ipv4Addr,              // IPv4 address of NIC via which radar can be reached
    pub spoke_data_addr: SocketAddrV4,   // Where the radar will send data spokes
    pub report_addr: SocketAddrV4,       // Where the radar will send reports
    pub send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
    pub legend: Legend,                  // What pixel values mean
    pub controls: SharedControls,        // Which controls there are, not complete in beginning
    pub range_detection: Option<RangeDetection>, // if Some, then ranges are flexible, detected and persisted
    rotation_timestamp: Instant,

    // Channels
    pub message_tx: tokio::sync::broadcast::Sender<Vec<u8>>, // Serialized RadarMessage
}

impl RadarInfo {
    pub fn new(
        locator_id: LocatorId,
        brand: Brand,
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
        controls: SharedControls,
    ) -> Self {
        let (message_tx, _message_rx) = tokio::sync::broadcast::channel(32);

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
            brand,
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
            legend: default_legend(false, pixel_values),
            message_tx,
            range_detection: None,
            controls,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
        }
    }

    pub fn all_clients_rx(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        self.controls.all_clients_rx()
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        self.controls.control_update_subscribe()
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn set_legend(&mut self, doppler: bool) {
        self.legend = default_legend(doppler, self.pixel_values);
    }

    pub fn full_rotation(&mut self) -> u32 {
        let now = Instant::now();
        let diff: Duration = now - self.rotation_timestamp;
        let diff = diff.as_millis() as f64;
        let rpm = format!("{:.0}", (600_000. / diff));

        self.rotation_timestamp = now;

        log::debug!(
            "{}: rotation speed elapsed {} = {} RPM",
            self.key,
            diff,
            rpm
        );

        if diff < 10000. && diff > 300. {
            let _ = self.controls.set_string(&ControlType::RotationSpeed, rpm);
            diff as u32
        } else {
            0
        }
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

        let mut rx = self.message_tx.subscribe();

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
            }
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

#[derive(Clone)]
pub struct SharedRadars {
    radars: Arc<RwLock<Radars>>,
}

impl SharedRadars {
    pub fn new() -> Self {
        SharedRadars {
            radars: Arc::new(RwLock::new(Radars {
                info: HashMap::new(),
                persistent_data: Persistence::new(),
            })),
        }
    }

    // A radar has been found
    pub fn located(&self, mut new_info: RadarInfo) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = self.radars.write().unwrap();

        // For now, drop second radar in replay Mode...
        if GLOBAL_ARGS.replay && key.ends_with("-B") {
            return None;
        }

        let max_radar_id = radars.info.iter().map(|(_, i)| i.id).max().unwrap_or(0);
        let max_persist_id = radars
            .persistent_data
            .config
            .radars
            .iter()
            .map(|(_, i)| i.id)
            .max()
            .unwrap_or(0);
        let max_id = std::cmp::max(max_radar_id, max_persist_id);

        let is_new = radars.info.get(&key).is_none();
        if is_new {
            // Set any previously detected model and ranges
            radars
                .persistent_data
                .update_info_from_persistence(&mut new_info);

            if new_info.id == usize::MAX {
                new_info.id = max_id + 1;
            }

            info!(
                "Found radar: key '{}' id {} name '{}'",
                &new_info.key,
                new_info.id,
                new_info.controls.user_name()
            );
            radars.info.insert(key, new_info.clone());
            Some(new_info)
        } else {
            None
        }
    }

    ///
    /// Update radar info in radars container
    ///
    pub fn update(&self, radar_info: &RadarInfo) {
        let mut radars = self.radars.write().unwrap();

        radars
            .info
            .insert(radar_info.key.clone(), radar_info.clone());

        radars.persistent_data.store(radar_info);
    }

    ///
    /// Return iterater over completed fully available radars
    ///
    pub fn get_active(&self) -> Vec<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars
            .info
            .iter()
            .map(|(_k, v)| v)
            .filter(|i| {
                i.range_detection.is_none()
                    || i.range_detection.as_ref().is_some_and(|r| r.complete)
            })
            .map(|v| v.clone())
            .collect()
    }

    pub fn find_radar_info(&self, key: &str) -> Result<RadarInfo, RadarError> {
        let radars = self.radars.read().unwrap();
        for info in radars.info.iter() {
            let id = format!("radar-{}", info.1.id);
            if id == key {
                return Ok(info.1.clone());
            }
        }
        Err(RadarError::NoSuchRadar(key.to_string()))
    }

    pub fn remove(&self, key: &str) {
        let mut radars = self.radars.write().unwrap();

        radars.info.remove(key);
    }

    ///
    /// Update radar info in radars container
    ///
    pub fn update_serial_no(&self, key: &str, serial_no: String) {
        let mut radars = self.radars.write().unwrap();

        if let Some(radar_info) = {
            if let Some(radar_info) = radars.info.get_mut(key) {
                if radar_info.serial_no != Some(serial_no.clone()) {
                    radar_info.serial_no = Some(serial_no);
                    Some(radar_info.clone())
                } else {
                    None
                }
            } else {
                None
            }
        } {
            radars.persistent_data.store(&radar_info);
        }
    }

    pub fn is_active_radar(&self, brand: &Brand, ip: &Ipv4Addr) -> bool {
        let radars = self.radars.read().unwrap();
        for (_, info) in radars.info.iter() {
            log::trace!(
                "is_active_radar: brand {}/{} ip {}/{}",
                info.brand,
                brand,
                info.nic_addr,
                ip
            );
            if info.brand == *brand && info.nic_addr == *ip {
                return true;
            }
        }
        false
    }
}

#[derive(Clone, Debug)]
struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub persistent_data: Persistence,
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
        strong_return: 0,
    };

    let mut pixel_values = pixel_values;
    if pixel_values > 255 - 32 - 2 {
        pixel_values = 255 - 32 - 2;
    }

    let pixels_with_color = pixel_values - 1;
    let one_third = pixels_with_color / 3;
    let two_thirds = one_third * 2;
    legend.strong_return = two_thirds;

    // No return is black
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
                r: if v >= two_thirds { 200 } else { 0 },
                // green starts at 1/3 and peaks at 2/3
                g: if v >= one_third && v < two_thirds {
                    200
                } else {
                    0
                },
                // blue peaks at 1/3
                b: if v < one_third { 200 } else { 0 },
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
                r: 200,
                g: 200,
                b: 0,
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
