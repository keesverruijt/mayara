use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use enum_primitive_derive::Primitive;
use log::info;
use protobuf::Message;
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

pub(crate) mod trail;

use crate::config::Persistence;
use crate::locator::LocatorId;
use crate::protos::RadarMessage::RadarMessage;
use crate::settings::{Control, ControlError, ControlMessage, ControlType, ControlValue, Controls};
use crate::Cli;

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
    #[error("Cannot set value for control '{0}'")]
    CannotSetControlType(ControlType),
    #[error("Missing value for control '{0}'")]
    MissingValue(ControlType),
    #[error("No such radar with key '{0}'")]
    NoSuchRadar(String),
    #[error("Cannot parse JSON '{0}'")]
    ParseJson(String),
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
#[derive(Clone, Debug)]
pub(crate) struct RadarInfo {
    key: String,
    pub id: usize,
    pub locator_id: LocatorId,
    pub brand: String,
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
    pub legend: Legend,                  // What pixel values mean
    pub range_detection: Option<RangeDetection>, // if Some, then ranges are flexible, detected and persisted
    pub controls: Controls, // Which controls there are, not complete in beginning
    // pub update: fn(&mut RadarInfo), // When controls or model is updated
    rotation_timestamp: Instant,

    // Channels
    pub message_tx: tokio::sync::broadcast::Sender<Vec<u8>>, // Serialized RadarMessage
    pub control_tx: tokio::sync::broadcast::Sender<ControlValue>,
    pub command_tx: tokio::sync::broadcast::Sender<ControlMessage>,
    pub protobuf_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
}

impl RadarInfo {
    pub fn new(
        locator_id: LocatorId,
        brand: &str,
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
        controls: Controls,
    ) -> Self {
        let (message_tx, _message_rx) = tokio::sync::broadcast::channel(32);
        let (control_tx, _control_rx) = tokio::sync::broadcast::channel(32);
        let (command_tx, _command_rx) = tokio::sync::broadcast::channel(32);
        let (protobuf_tx, _protobuf_rx) = tokio::sync::broadcast::channel(32);

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
            control_tx,
            command_tx,
            protobuf_tx,
            range_detection: None,
            controls,
            rotation_timestamp: Instant::now() - Duration::from_secs(2),
        }
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

        log::debug!("{}: rotation speed {} dRPM", self.key, rpm);

        if diff < 3000. && diff > 600. {
            let _ = self
                .command_tx
                .send(ControlMessage::SetValue(ControlValue::new(
                    ControlType::RotationSpeed,
                    rpm,
                )));
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

    pub async fn send_all_json(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        for c in self.controls.iter() {
            self.send_json(reply_tx.clone(), c, None).await?;
        }
        Ok(())
    }

    pub fn set_refresh(&mut self, control_type: &ControlType) {
        if let Some(control) = self.controls.get_mut(control_type) {
            control.needs_refresh = true;
        }
    }

    pub fn set_value_auto_enabled(
        &mut self,
        control_type: &ControlType,
        value: f32,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            if let Some(control) = self.controls.get_mut(control_type) {
                Ok(control
                    .set(value, None, auto, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.broadcast_protobuf(&control);
            self.broadcast_json(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set(
        &mut self,
        control_type: &ControlType,
        value: f32,
        auto: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            if let Some(control) = self.controls.get_mut(control_type) {
                Ok(control
                    .set(value, None, auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.broadcast_protobuf(&control);
            self.broadcast_json(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_auto_state(
        &mut self,
        control_type: &ControlType,
        auto: bool,
    ) -> Result<(), ControlError> {
        if let Some(control) = self.controls.get_mut(control_type) {
            control.set_auto(auto);
        } else {
            return Err(ControlError::NotSupported(*control_type));
        };
        Ok(())
    }

    pub fn set_value_auto(
        &mut self,
        control_type: &ControlType,
        auto: bool,
        value: f32,
    ) -> Result<Option<()>, ControlError> {
        self.set(control_type, value, Some(auto))
    }

    pub fn set_value_with_many_auto(
        &mut self,
        control_type: &ControlType,
        value: f32,
        auto_value: f32,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            if let Some(control) = self.controls.get_mut(control_type) {
                let auto = control.auto;
                Ok(control
                    .set(value, Some(auto_value), auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.broadcast_protobuf(&control);
            self.broadcast_json(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(
        &mut self,
        control_type: &ControlType,
        value: String,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            if let Some(control) = self.controls.get_mut(control_type) {
                if control.item().is_string_value {
                    Ok(control.set_string(value).map(|_| control.clone()))
                } else {
                    let i = value
                        .parse::<i32>()
                        .map_err(|_| ControlError::Invalid(control_type.clone(), value))?;
                    control
                        .set(i as f32, None, None, None)
                        .map(|_| Some(control.clone()))
                }
            } else {
                Err(ControlError::NotSupported(*control_type))
            }
        }?;

        if let Some(control) = control {
            self.broadcast_protobuf(&control);
            self.broadcast_json(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    fn get_description(control: &Control) -> Option<String> {
        if let (Some(value), Some(descriptions)) = (control.value, &control.item().descriptions) {
            let value = value as i32;
            if value >= 0 && value < (descriptions.len() as i32) {
                return descriptions.get(&value).cloned();
            }
        }
        return None;
    }

    fn broadcast_json(&self, control: &Control) {
        let control_value = crate::settings::ControlValue {
            id: control.item().control_type,
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            error: None,
        };

        match self.control_tx.send(control_value) {
            Err(_e) => {}
            Ok(cnt) => {
                log::trace!(
                    "Sent control value {} to {} JSON clients",
                    control.item().control_type,
                    cnt
                );
            }
        }
    }

    pub(super) async fn send_json(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        control: &Control,
        error: Option<String>,
    ) -> Result<(), RadarError> {
        let control_value = crate::settings::ControlValue {
            id: control.item().control_type,
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            error,
        };

        match reply_tx.send(control_value).await {
            Err(_e) => {}
            Ok(()) => {
                log::trace!(
                    "Sent control value {} to requestng JSON client",
                    control.item().control_type,
                );
            }
        }

        Ok(())
    }

    fn broadcast_protobuf(&self, control: &Control) {
        if let Some(value) = control.value {
            let mut control_value = crate::protos::RadarMessage::radar_message::ControlValue::new();
            control_value.id = control.item().control_type.to_string();
            control_value.value = value;
            control_value.auto = control.auto;
            control_value.description = Self::get_description(control);

            let mut message = RadarMessage::new();
            message.controls.push(control_value);

            let mut bytes = Vec::new();
            message
                .write_to_vec(&mut bytes)
                .expect("Cannot write RadarMessage to vec");
            match self.protobuf_tx.send(bytes) {
                Err(_e) => {
                    log::trace!(
                        "Stored control value {} value {}",
                        control.item().control_type,
                        &value
                    );
                }
                Ok(cnt) => {
                    log::trace!(
                        "Stored control value {} value {} and sent to {} clients",
                        control.item().control_type,
                        &value,
                        cnt
                    );
                }
            }
        }
    }

    pub fn user_name(&self) -> String {
        return self.controls.user_name().unwrap_or_else(|| self.key());
    }

    pub fn set_user_name(&mut self, name: String) {
        self.controls.set_user_name(name);
    }

    pub fn set_model_name(&mut self, name: String) {
        self.controls.set_model_name(name);
    }

    pub fn model_name(&self) -> Option<String> {
        self.controls.model_name()
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
    pub fn new(args: Cli) -> Self {
        SharedRadars {
            radars: Arc::new(RwLock::new(Radars {
                info: HashMap::new(),
                args,
                persistent_data: Persistence::new(),
            })),
        }
    }

    // A radar has been found
    pub fn located(&self, mut new_info: RadarInfo) -> Option<RadarInfo> {
        let key = new_info.key.to_owned();
        let mut radars = self.radars.write().unwrap();

        // For now, drop second radar in replay Mode...
        if radars.args.replay && key.ends_with("-B") {
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
                "Located a new radar: key '{}' id {} name '{}'",
                &new_info.key,
                new_info.id,
                new_info.user_name()
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

    pub fn cli_args(&self) -> Cli {
        let radars = self.radars.read().unwrap();
        radars.args.clone()
    }
}

#[derive(Clone, Debug)]
pub struct Radars {
    pub info: HashMap<String, RadarInfo>,
    pub args: Cli,
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

    const WHITE: f32 = 256.0;
    let pixels_with_color = pixel_values - 1;
    let start = WHITE / 3.0;
    let delta: f32 = (WHITE * 2.0) / (pixels_with_color as f32);
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
                r: if v >= two_thirds {
                    (start + ((v - two_thirds) as f32) * delta) as u8
                } else {
                    0
                },
                // green starts at 1/3 and peaks at 2/3
                g: if v >= one_third && v < two_thirds {
                    (start + ((v - one_third) as f32) * delta) as u8
                } else {
                    0
                },
                // blue peaks at 1/3
                b: if v < one_third {
                    (start + (v as f32) * (WHITE / (pixel_values as f32))) as u8
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
