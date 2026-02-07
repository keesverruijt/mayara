use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_primitive_derive::Primitive;
use protobuf::Message;
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};
use serde_json::Value;
use std::time::{Duration, Instant};
use std::{
    collections::HashMap,
    fmt::{self, Display, Write},
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};

pub mod range;
pub mod spoke;
pub mod target;
pub mod trail;

use crate::brand::CommandSender;
use crate::config::Persistence;
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::trail::TrailBuffer;
use crate::settings::{
    ControlDestination, ControlError, ControlType, ControlUpdate, ControlValue, RadarControlValue,
    SharedControls,
};
use crate::{Brand, Cli, TargetMode};
use range::{RangeDetection, Ranges};

pub const NAUTICAL_MILE: i32 = 1852; // 1 nautical mile in meters
pub const NAUTICAL_MILE_F64: f64 = 1852.; // 1 nautical mile in meters

// A "native to radar" bearing, usually [0..2048] or [0..4096] or [0..8192]
pub type SpokeBearing = u16;

pub const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

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
    #[error("No such control '{0}'")]
    InvalidControlType(String),
    #[error("{0}")]
    ControlError(#[from] ControlError),
    #[error("Cannot set value for control '{0}'")]
    CannotSetControlType(ControlType),
    #[error("Cannot control '{0}' to value {1}")]
    CannotSetControlTypeValue(ControlType, Value),
    #[error("Missing value for control '{0}'")]
    MissingValue(ControlType),
    #[error("No such radar with key '{0}'")]
    NoSuchRadar(String),
    #[error("Cannot parse JSON '{0}'")]
    ParseJson(String),
    #[error("Cannot parse NMEA0183 '{0}'")]
    ParseNmea0183(String),
    #[error("IP address changed")]
    IPAddressChanged,
    #[error("Cannot login to radar")]
    LoginFailed,
    #[error("Invalid port number")]
    InvalidPort,
    #[error("Not connected")]
    NotConnected,
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
    pub target_colors: u8,
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

/// A geographic position expressed in degrees latitude and longitude.
/// Latitude is positive in the northern hemisphere, negative in the southern.
/// Longitude is positive in the eastern hemisphere, negative in the western.
/// The range for latitude is -90 to 90, and for longitude is -180 to 180.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GeoPosition {
    lat: f64,
    lon: f64,
}

impl GeoPosition {
    pub fn new(lat: f64, lon: f64) -> Self {
        GeoPosition { lat, lon }
    }
}

impl fmt::Display for GeoPosition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.lat, self.lon)
    }
}

#[derive(Clone, Debug)]
pub struct RadarInfo {
    key: String,

    // selected items from Cli args:
    targets: TargetMode,
    replay: bool,
    output: bool,

    pub id: usize,
    pub brand: Brand,
    pub serial_no: Option<String>,       // Serial # for this radar
    pub which: Option<String>,           // "A", "B" or None
    pub pixel_values: u8,                // How many values per pixel, 0..220 or so
    pub spokes_per_revolution: u16,      // How many spokes per rotation
    pub max_spoke_len: u16,              // Fixed for some radars, variable for others
    pub addr: SocketAddrV4,              // The IP address of the radar
    pub nic_addr: Ipv4Addr,              // IPv4 address of NIC via which radar can be reached
    pub spoke_data_addr: SocketAddrV4,   // Where the radar will send data spokes
    pub report_addr: SocketAddrV4,       // Where the radar will send reports
    pub send_command_addr: SocketAddrV4, // Where displays will send commands to the radar
    pub legend: Legend,                  // What pixel values mean
    pub controls: SharedControls,        // Which controls there are, not complete in beginning
    pub ranges: Ranges,                  // Ranges for this radar, empty in beginning
    pub(crate) range_detection: Option<RangeDetection>, // if Some, then ranges are flexible, detected and persisted
    pub doppler: bool,                                  // Does it support Doppler?
    pub dual_range: bool,                               // Is it dual range capable?
    rotation_timestamp: Instant,

    // Channels
    pub message_tx: tokio::sync::broadcast::Sender<Vec<u8>>, // Serialized RadarMessage
}

impl RadarInfo {
    pub fn new(
        args: &Cli,
        brand: Brand,
        serial_no: Option<&str>,
        which: Option<&str>,
        pixel_values: u8, // How many values per pixel, 0..220 or so
        spokes_per_revolution: usize,
        max_spoke_len: usize,
        addr: SocketAddrV4,
        nic_addr: Ipv4Addr,
        spoke_data_addr: SocketAddrV4,
        report_addr: SocketAddrV4,
        send_command_addr: SocketAddrV4,
        controls: SharedControls,
        doppler: bool,
    ) -> Self {
        let (message_tx, _message_rx) = tokio::sync::broadcast::channel(32);

        let (targets, replay, output) = {
            (
                args.targets.clone(),
                args.replay.clone(),
                args.output.clone(),
            )
        };
        let legend = default_legend(&targets, false, pixel_values);

        let info = RadarInfo {
            targets,
            replay,
            output,
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
            brand,
            serial_no: serial_no.map(String::from),
            which: which.map(String::from),
            pixel_values,
            spokes_per_revolution: spokes_per_revolution as u16,
            max_spoke_len: max_spoke_len as u16,
            addr,
            nic_addr,
            spoke_data_addr,
            report_addr,
            send_command_addr,
            legend: legend,
            message_tx,
            ranges: Ranges::empty(),
            range_detection: None,
            controls,
            doppler,
            dual_range: false,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
        };

        log::info!("Created RadarInfo {:?}", info);
        info
    }

    pub fn new_client_subscription(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        self.controls.new_client_subscription()
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        self.controls.control_update_subscribe()
    }

    pub fn key(&self) -> String {
        self.key.to_owned()
    }

    pub fn set_doppler(&mut self, doppler: bool) {
        if doppler != self.doppler {
            self.legend = default_legend(&self.targets, doppler, self.pixel_values);
            log::info!("Doppler changed to {}", doppler);
            self.doppler = doppler;
        }
    }

    pub fn set_pixel_values(&mut self, pixel_values: u8) {
        if pixel_values != self.pixel_values {
            self.legend = default_legend(&self.targets, self.doppler, pixel_values);
            log::info!("Pixel_values changed to {}", pixel_values);
        }
        self.pixel_values = pixel_values;
    }

    pub fn set_rotation_length(&mut self, millis: u32) -> u32 {
        let diff = millis as f64;
        let rpm = format!("{:.0}", (600_000. / diff));

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

    pub fn set_ranges(&mut self, ranges: Ranges) -> Result<(), RadarError> {
        self.controls
            .set_valid_ranges(&ControlType::Range, &ranges)?;
        self.ranges = ranges;
        Ok(())
    }

    pub fn broadcast_radar_message(&self, message: RadarMessage) {
        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");

        // Send the message to all receivers, normally the web client(s)
        // We send raw bytes to avoid encoding overhead in each web client.
        // This strategy will change when clients want different protocols.
        match self.message_tx.send(bytes) {
            Err(e) => {
                log::trace!("{}: Dropping received spoke: {}", self.key, e);
            }
            Ok(count) => {
                log::trace!("{}: sent to {} receivers", self.key, count);
            }
        }
    }

    pub fn start_forwarding_radar_messages_to_stdout(&self, subsys: &SubsystemHandle) {
        if self.output {
            let info_clone2 = self.clone();

            subsys.start(SubsystemBuilder::new("stdout", move |s| {
                info_clone2.forward_output(s)
            }));
        }
    }

    async fn forward_output(self, subsys: SubsystemHandle) -> Result<(), RadarError> {
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
        write!(f, "Radar {} brand {}", &self.id, &self.brand)?;
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
        let (sk_client_tx, _) = tokio::sync::broadcast::channel(32);

        SharedRadars {
            radars: Arc::new(RwLock::new(Radars {
                info: HashMap::new(),
                persistent_data: Persistence::new(),
                sk_client_tx,
            })),
        }
    }

    // A radar has been found
    pub fn located(&self, mut new_info: RadarInfo) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = self.radars.write().unwrap();

        // For now, drop second radar in replay Mode...
        if new_info.replay && key.ends_with("-B") {
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

            log::info!("key '{}' info {:?}", &new_info.key, new_info);
            log::info!(
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
    pub fn update(&self, radar_info: &mut RadarInfo) {
        let mut radars = self.radars.write().unwrap();

        let sk_client_tx = radars.sk_client_tx.clone();
        radar_info
            .controls
            .set_radar_info(sk_client_tx, radar_info.key());
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
            .filter(|i| i.ranges.len() > 0)
            .map(|v| v.clone())
            .collect()
    }

    pub fn have_active(&self) -> bool {
        let radars = self.radars.read().unwrap();
        radars
            .info
            .iter()
            .map(|(_k, v)| v)
            .filter(|i| i.ranges.len() > 0)
            .count()
            > 0
    }

    #[allow(dead_code)]
    pub fn get_by_key(&self, key: &str) -> Option<RadarInfo> {
        let radars = self.radars.read().unwrap();
        radars.info.get(key).cloned()
    }

    pub fn get_by_id(&self, key: &str) -> Option<RadarInfo> {
        let radars = self.radars.read().unwrap();
        for info in radars.info.iter() {
            let id = format!("radar-{}", info.1.id);
            if id == key {
                return Some(info.1.clone());
            }
        }
        None
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

    pub fn new_sk_client_subscription(
        &self,
    ) -> tokio::sync::broadcast::Receiver<RadarControlValue> {
        self.radars.read().unwrap().sk_client_tx.subscribe()
    }
}

#[derive(Clone, Debug)]
struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub persistent_data: Persistence,
    sk_client_tx: tokio::sync::broadcast::Sender<RadarControlValue>,
}

pub struct Statistics {
    pub broken_packets: usize,
    pub missing_spokes: usize,  // this revolution
    pub received_spokes: usize, // this revolution
    pub total_rotations: usize, // total number of revolutions
}

impl Statistics {
    pub fn new() -> Self {
        Statistics {
            broken_packets: 0,
            missing_spokes: 0,
            received_spokes: 0,
            total_rotations: 0,
        }
    }

    pub fn full_rotation(&mut self, key: &str) {
        self.total_rotations += 1;
        log::debug!(
            "{}: Full rotation #{},  {} spokes received and {} missing spokes {} broken packets",
            key,
            self.total_rotations,
            self.received_spokes,
            self.missing_spokes,
            self.broken_packets
        );
        self.received_spokes = 0;
        self.missing_spokes = 0;
        self.broken_packets = 0;
    }
}

#[derive(Debug, PartialEq)]
pub enum Power {
    Off,
    Standby,
    Transmit,
    Preparing,
}

impl fmt::Display for Power {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl Power {
    pub(crate) fn from_value(s: &Value) -> Result<Self, RadarError> {
        match s {
            Value::Number(n) => match n.as_i64() {
                Some(0) => Ok(Power::Off),
                Some(1) => Ok(Power::Standby),
                Some(2) => Ok(Power::Transmit),
                Some(3) => Ok(Power::Preparing),
                _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
            },
            Value::String(s) => match s.to_ascii_lowercase().as_str() {
                "0" | "off" => Ok(Power::Off),
                "1" | "standby" => Ok(Power::Standby),
                "2" | "transmit" => Ok(Power::Transmit),
                "3" | "preparing" => Ok(Power::Preparing),
                _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
            },
            _ => Err(RadarError::ParseJson(format!("Unknown status: {}", s))),
        }
    }
}

// The actual values are not arbitrary: these are the exact values as reported
// by HALO radars, simplifying the navico::report code.
#[derive(Copy, Clone, Debug, Primitive, PartialEq)]
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

fn default_legend(targets: &TargetMode, doppler: bool, pixel_values: u8) -> Legend {
    let mut legend = Legend {
        pixels: Vec::new(),
        target_colors: 0,
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

    // No return is black
    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: TRANSPARENT,
        },
    });
    legend.target_colors = pixel_values;
    if pixel_values == 0 {
        return legend;
    }

    let pixels_with_color = pixel_values - 1;
    let one_third = pixels_with_color / 3;
    let two_thirds = one_third * 2;
    legend.strong_return = two_thirds;

    for v in 1..pixel_values {
        legend.pixels.push(Lookup {
            r#type: PixelType::Normal,
            color: Color {
                // red starts at 2/3 and peaks at end
                r: if v >= two_thirds {
                    (255.0 * (v - two_thirds) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                // green starts at 1/3 and peaks at 2/3
                g: if v >= one_third && v < two_thirds {
                    (255.0 * (v - one_third) as f64 / one_third as f64) as u8
                } else if v >= two_thirds {
                    (255.0 * (pixels_with_color - v) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                // blue peaks at 1/3
                b: if v < one_third {
                    (255.0 * v as f64 / one_third as f64) as u8
                } else if v >= one_third && v < two_thirds {
                    (255.0 * (two_thirds - v) as f64 / one_third as f64) as u8
                } else {
                    0
                },
                a: OPAQUE,
            },
        });
    }

    legend.pixels.push(Lookup {
        r#type: PixelType::Normal,
        color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: OPAQUE,
        },
    });

    if *targets == TargetMode::Arpa {
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
    }

    if doppler {
        legend.doppler_approaching = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerApproaching,
            color: Color {
                // Purple
                r: 255,
                g: 0,
                b: 255,
                a: OPAQUE,
            },
        });
        legend.doppler_receding = legend.pixels.len() as u8;
        legend.pixels.push(Lookup {
            r#type: PixelType::DopplerReceding,
            color: Color {
                // Green
                r: 0x00,
                g: 0xff,
                b: 0x00,
                a: OPAQUE,
            },
        });
    }

    if *targets != TargetMode::None {
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
    }

    log::info!("Created legend {:?}", legend);
    legend
}

#[cfg(test)]
mod tests {
    use super::default_legend;

    #[test]
    fn legend() {
        let targets = crate::TargetMode::Arpa;
        let legend = default_legend(&targets, true, 16);
        let json = serde_json::to_string_pretty(&legend).unwrap();
        println!("{}", json);
    }
}

pub(crate) struct CommonRadar {
    pub key: String,
    pub info: RadarInfo,
    radars: SharedRadars,
    pub statistics: Statistics,
    pub trails: TrailBuffer,
    pub control_update_rx: broadcast::Receiver<ControlUpdate>,
    pub replay: bool,
}

impl CommonRadar {
    pub fn new(
        key: String,
        info: RadarInfo,
        radars: SharedRadars,
        trails: TrailBuffer,
        control_update_rx: broadcast::Receiver<ControlUpdate>,
        replay: bool,
    ) -> Self {
        CommonRadar {
            key,
            info,
            radars,
            statistics: Statistics::new(),
            trails,
            control_update_rx,
            replay,
        }
    }

    pub(crate) fn update(&mut self) {
        self.radars.update(&mut self.info);
    }

    pub async fn process_control_update<T: CommandSender>(
        &mut self,
        control_update: ControlUpdate,
        command_sender: &mut Option<T>,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        if let Some(control) = self.info.controls.get(&cv.id) {
            match &control.item().destination {
                ControlDestination::Internal => {
                    panic!("ControlType::Internal should not be sent to radar receiver")
                }
                ControlDestination::Trail => {
                    match self.trails.set_control_value(&self.info.controls, &cv) {
                        Ok(()) => {
                            return Ok(());
                        }
                        Err(e) => {
                            return self
                                .info
                                .controls
                                .send_error_to_client(reply_tx, &cv, &e)
                                .await;
                        }
                    };
                }
                ControlDestination::Command => {
                    if let Some(command_sender) = command_sender {
                        if let Err(e) = command_sender.set_control(&cv, &self.info.controls).await {
                            return self
                                .info
                                .controls
                                .send_error_to_client(reply_tx, &cv, &e)
                                .await;
                        } else {
                            self.info.controls.set_refresh(&cv.id);
                        }
                    }
                }
            }
        } else {
            panic!("Unhandled control {} sent to radar receiver", cv.id);
        }

        Ok(())
    }
}
