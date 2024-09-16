use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::FromPrimitive;
use serde::{Deserialize, Deserializer, Serialize};
use serde_repr::*;
use std::{
    collections::HashMap,
    fmt::{self, Display},
    str::FromStr,
};
use thiserror::Error;

///
/// Radars have settings. There are some common ones that every radar supports:
/// range, gain, sea clutter and rain clutter. Some others are less common, and
/// are usually expressed in slightly different ways.
/// For instance, a radar may have an interference rejection setting. Some will
/// have two possible values (off or on) whilst others may have multiple levels,
/// like off, low, medium and high.
///
/// To cater for this, we keep the state of these settings in generalized state
/// structures in Rust.
///

#[derive(Clone, Copy, Debug, Serialize)]
pub enum ControlState {
    Off,
    Manual,
    Auto, // TODO: radar_pi had multiple for Garmin, lets see if we can do this better
}

impl Display for ControlState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                ControlState::Off => "Off",
                ControlState::Manual => "Manual",
                ControlState::Auto => "Auto",
            }
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Controls {
    #[serde(flatten)]
    controls: HashMap<ControlType, Control>,
}

impl Controls {
    pub fn get(&self, control_type: &ControlType) -> Option<&Control> {
        self.controls.get(control_type)
    }

    pub fn get_mut(&mut self, control_type: &ControlType) -> Option<&mut Control> {
        self.controls.get_mut(control_type)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Control> {
        self.controls.iter().map(|(_k, v)| v)
    }

    pub fn insert(&mut self, control_type: ControlType, value: Control) {
        self.controls.insert(control_type, value);
    }

    pub fn new_base(controls: HashMap<ControlType, Control>) -> Self {
        Controls { controls }
    }

    pub fn set_user_name(&mut self, name: String) {
        let control = self.get_mut(&ControlType::UserName).unwrap();
        control.set_string(name);
    }

    pub fn user_name(&self) -> Option<String> {
        if let Some(control) = self.controls.get(&ControlType::UserName) {
            return control.description.clone();
        }
        return None;
    }

    pub fn set_model_name(&mut self, name: String) {
        let control = self.controls.get_mut(&ControlType::ModelName).unwrap();
        control.set_string(name.clone());
    }

    pub fn model_name(&self) -> Option<String> {
        if let Some(control) = self.controls.get(&ControlType::ModelName) {
            return control.description.clone();
        }
        return None;
    }
}

#[derive(Clone, Debug)]
pub enum ControlMessage {
    Value(tokio::sync::mpsc::Sender<ControlValue>, ControlValue),
    NewClient(tokio::sync::mpsc::Sender<ControlValue>),
}

// This is what we send back and forth to clients
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    #[serde(deserialize_with = "deserialize_enum_from_string")]
    pub id: ControlType,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlValue {
    pub fn new(id: ControlType, value: String) -> Self {
        ControlValue {
            id,
            value,
            auto: None,
            error: None,
        }
    }

    pub(crate) fn new_error(id: ControlType, value: String, error: String) -> Self {
        ControlValue {
            id,
            value,
            auto: None,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Control {
    #[serde(flatten)]
    item: ControlDefinition,
    #[serde(skip)]
    pub value: Option<i32>,
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(skip)]
    pub auto: Option<bool>,
    #[serde(skip)]
    pub state: ControlState,
    #[serde(skip)]
    pub needs_refresh: bool, // True when it has been changed and client needs to know value (again)
}

impl Control {
    fn new(item: ControlDefinition) -> Self {
        let value = item.default_value.clone();
        Control {
            item,
            value,
            auto: None,
            state: ControlState::Off,
            description: None,
            needs_refresh: false,
        }
    }

    // pub fn step(mut self, step: i32) -> Self {
    //     self.item.step_value = step;

    //     self
    // }

    pub fn read_only(mut self, is_read_only: bool) -> Self {
        self.item.is_read_only = is_read_only;

        self
    }

    pub fn wire_scale_factor(mut self, wire_scale_factor: i32) -> Self {
        self.item.wire_scale_factor = Some(wire_scale_factor);

        self
    }

    pub fn wire_offset(mut self, wire_offset: i32) -> Self {
        self.item.wire_offset = Some(wire_offset);

        self
    }

    pub fn unit<S: AsRef<str>>(mut self, unit: S) -> Control {
        self.item.unit = Some(unit.as_ref().to_string());

        self
    }

    pub fn new_numeric(control_type: ControlType, min_value: i32, max_value: i32) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        let control = Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_off: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: Some(1),
            wire_scale_factor: max_value,
            wire_offset: Some(0),
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
        });
        control
    }

    pub fn new_auto(
        control_type: ControlType,
        min_value: i32,
        max_value: i32,
        automatic: AutomaticValue,
    ) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: Some(automatic),
            has_off: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: Some(1),
            wire_scale_factor: max_value,
            wire_offset: Some(0),
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
        })
    }

    pub fn new_list(control_type: ControlType, descriptions: &[&str]) -> Self {
        Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_off: false,
            default_value: Some(0),
            min_value: Some(0),
            max_value: Some((descriptions.len() as i32) - 1),
            step_value: Some(1),
            wire_scale_factor: Some((descriptions.len() as i32) - 1),
            wire_offset: Some(0),
            unit: None,
            descriptions: Some(
                descriptions
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| (i as i32, n.to_string()))
                    .collect(),
            ),
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
        })
    }

    pub fn new_string(control_type: ControlType) -> Self {
        let control = Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_off: false,
            default_value: None,
            min_value: None,
            max_value: None,
            step_value: None,
            wire_scale_factor: None,
            wire_offset: None,
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: true,
            is_string_value: true,
        });
        control
    }

    /// Read-only access to the definition of the control
    pub fn item(&self) -> &ControlDefinition {
        &self.item
    }

    pub fn set_valid_values(&mut self, values: Vec<i32>) {
        self.item.valid_values = Some(values);

        if let Some(unit) = &self.item.unit {
            if unit == "m" {
                // Reset the descriptions map
                let mut descriptions = HashMap::new();
                for v in self.item.valid_values.as_ref().unwrap().iter() {
                    let mut desc = if v % 25 == 0 {
                        // Metric
                        if v % 1000 == 0 {
                            format!("{} km", v / 1000)
                        } else {
                            format!("{} m", v)
                        }
                    } else {
                        if v % 1852 == 0 {
                            format!("{} nm", v / 1852)
                        } else {
                            match v {
                                57 => "1/32 nm",
                                114 => "1/16 nm",
                                231 => "1/8 nm",
                                463 => "1/4 nm",
                                926 => "1/2 nm",
                                1389 => "3/4 nm",
                                2778 => "1.5 nm",
                                _ => "",
                            }
                            .to_string()
                        }
                    };
                    if desc.len() == 0 {
                        desc = format!("{} m", v);
                    }
                    descriptions.insert(*v, desc);
                }
                self.item.descriptions = Some(descriptions);
            }
        }
    }

    // pub fn auto(&self) -> Option<bool> {
    //     self.auto
    // }

    pub fn value(&self) -> String {
        if self.item.is_string_value {
            return self.description.clone().unwrap_or_else(|| "".to_string());
        }

        self.value
            .unwrap_or(self.item.default_value.unwrap_or(0))
            .to_string()
    }

    pub fn set_all(
        &mut self,
        mut value: i32,
        auto: Option<bool>,
        state: ControlState,
    ) -> Result<Option<()>, ControlError> {
        if let (Some(wire_scale_factor), Some(max_value)) =
            (self.item.wire_scale_factor, self.item.max_value)
        {
            if wire_scale_factor != max_value {
                value = (((value as i64) * (max_value as i64)) / (wire_scale_factor as i64)) as i32;
            }
        }
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.is_none() && value > max_value && value <= 2 * max_value {
                // debug!("{} value {} -> ", self.item.control_type, value);
                value -= 2 * max_value;
                // debug!("{} ..... {}", self.item.control_type, value);
            }

            if value < min_value {
                return Err(ControlError::TooLow(
                    self.item.control_type,
                    value,
                    min_value,
                ));
            }
            if value > max_value {
                return Err(ControlError::TooHigh(
                    self.item.control_type,
                    value,
                    max_value,
                ));
            }
        }

        if auto.is_some() && self.item.automatic.is_none() {
            Err(ControlError::NoAuto(self.item.control_type))
        } else if self.value != Some(value) || self.auto != auto {
            self.value = Some(value);
            self.auto = auto;
            self.state = state;
            self.needs_refresh = false;

            Ok(Some(()))
        } else if self.needs_refresh {
            self.needs_refresh = false;
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(&mut self, value: String) -> Option<()> {
        let value = Some(value);
        if &self.description != &value {
            self.description = value;
            self.needs_refresh = false;
            self.state = ControlState::Manual;
            log::trace!("Set {} to {:?}", self.item.control_type, self.description);
            Some(())
        } else if self.needs_refresh {
            self.needs_refresh = false;
            Some(())
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AutomaticValue {
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_auto: bool,
    #[serde(skip)]
    pub(crate) auto_values: i32,
    #[serde(skip)]
    pub(crate) auto_descriptions: Option<Vec<String>>,
    pub(crate) has_auto_adjustable: bool,
    pub(crate) auto_adjust_min_value: i32,
    pub(crate) auto_adjust_max_value: i32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControlDefinition {
    #[serde(skip)]
    pub(crate) control_type: ControlType,
    name: String,
    #[serde(skip)]
    has_off: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    automatic: Option<AutomaticValue>,
    #[serde(skip_serializing_if = "is_false")]
    is_string_value: bool,
    #[serde(skip)]
    default_value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_value: Option<i32>,
    #[serde(skip)]
    wire_scale_factor: Option<i32>,
    #[serde(skip)]
    wire_offset: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptions: Option<HashMap<i32, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) valid_values: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "is_false")]
    is_read_only: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {}

#[derive(
    Eq,
    PartialEq,
    Hash,
    Copy,
    Clone,
    Debug,
    Serialize_repr,
    Deserialize_repr,
    FromPrimitive,
    ToPrimitive,
)]
#[repr(u8)]
pub enum ControlType {
    Status,
    Range,
    Gain,
    Mode,
    // AllAuto,
    Rain,
    Sea,
    SeaState,
    // Scaling,
    ScanSpeed,
    Doppler,
    // DopplerAutoTrack,
    DopplerSpeedThreshold,
    SideLobeSuppression,
    // Stc,
    // StcCurve,
    TargetBoost,
    TargetExpansion,
    TargetSeparation,
    // TargetTrails,
    // TimedIdle,
    // TimedRun,
    // TrailsMotion,
    // TuneCoarse,
    // TuneFine,
    // ColorGain,
    // DisplayTiming,
    // Ftc,
    InterferenceRejection,
    NoiseRejection,
    LocalInterferenceRejection,
    // MainBangSize,
    // MainBangSuppression,
    NoTransmitEnd1,
    NoTransmitEnd2,
    NoTransmitEnd3,
    NoTransmitEnd4,
    NoTransmitStart1,
    NoTransmitStart2,
    NoTransmitStart3,
    NoTransmitStart4,
    AccentLight,
    // AntennaForward,
    AntennaHeight,
    // AntennaStarboard,
    BearingAlignment,
    OperatingHours,
    ModelName,
    FirmwareVersion,
    SerialNumber,
    UserName,
    // Orientation,
}

impl Display for ControlType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            ControlType::AccentLight => "Accent light",
            // ControlType::AllAuto => "All to Auto",
            // ControlType::AntennaForward => "Antenna forward of GPS",
            ControlType::AntennaHeight => "Antenna height",
            // ControlType::AntennaStarboard => "Antenna starboard of GPS",
            ControlType::BearingAlignment => "Bearing alignment",
            // ControlType::ColorGain => "Color gain",
            // ControlType::DisplayTiming => "Display timing",
            ControlType::Doppler => "Doppler",
            // ControlType::DopplerAutoTrack => "Doppler Auto Track",
            ControlType::DopplerSpeedThreshold => "Doppler speed threshold",
            ControlType::FirmwareVersion => "Firmware version",
            // ControlType::Ftc => "FTC",
            ControlType::Gain => "Gain",
            ControlType::InterferenceRejection => "Interference rejection",
            ControlType::LocalInterferenceRejection => "Local interference rejection",
            // ControlType::MainBangSize => "Main bang size",
            // ControlType::MainBangSuppression => "Main bang suppression",
            ControlType::Mode => "Mode",
            ControlType::ModelName => "Model name",
            ControlType::NoTransmitEnd1 => "No Transmit end",
            ControlType::NoTransmitEnd2 => "No Transmit end (2)",
            ControlType::NoTransmitEnd3 => "No Transmit end (3)",
            ControlType::NoTransmitEnd4 => "No Transmit end (4)",
            ControlType::NoTransmitStart1 => "No Transmit start",
            ControlType::NoTransmitStart2 => "No Transmit start (2)",
            ControlType::NoTransmitStart3 => "No Transmit start (3)",
            ControlType::NoTransmitStart4 => "No Transmit start (4)",
            ControlType::NoiseRejection => "Noise rejection",
            ControlType::OperatingHours => "Operating hours",
            // ControlType::Orientation => "Orientation",
            ControlType::Rain => "Rain clutter",
            ControlType::Range => "Range",
            // ControlType::Scaling => "Scaling",
            ControlType::ScanSpeed => "Fast scan",
            ControlType::Sea => "Sea clutter",
            ControlType::SeaState => "Sea state",
            ControlType::SerialNumber => "Serial Number",
            ControlType::SideLobeSuppression => "Side lobe suppression",
            // ControlType::Stc => "Sensitivity Time Control",
            // ControlType::StcCurve => "STC curve",
            ControlType::Status => "Status",
            ControlType::TargetBoost => "Target boost",
            ControlType::TargetExpansion => "Target expansion",
            ControlType::TargetSeparation => "Target separation",
            // ControlType::TargetTrails => "Target trails",
            // ControlType::TimedIdle => "Time idle",
            // ControlType::TimedRun => "Timed run",
            // ControlType::TrailsMotion => "Target trails motion",
            // ControlType::TuneCoarse => "Coarse tune",
            // ControlType::TuneFine => "Fine tune",
            ControlType::UserName => "Custom name",
        };

        write!(f, "{}", s)
    }
}

#[derive(Error, Debug)]
pub enum ControlError {
    #[error("Control {0} not supported on this radar")]
    NotSupported(ControlType),
    #[error("Control {0} value {1} is lower than minimum value {2}")]
    TooLow(ControlType, i32, i32),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(ControlType, i32, i32),
    #[error("Control {0} does not support Auto")]
    NoAuto(ControlType),
}

pub fn deserialize_number_from_string<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr + serde::Deserialize<'de>,
    <T as FromStr>::Err: Display,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt<T> {
        String(String),
        Number(T),
    }

    match StringOrInt::<T>::deserialize(deserializer)? {
        StringOrInt::String(s) => s.parse::<T>().map_err(serde::de::Error::custom),
        StringOrInt::Number(i) => Ok(i),
    }
}

pub fn deserialize_enum_from_string<'de, T, D>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: FromPrimitive + serde::Deserialize<'de>,
{
    let n = deserialize_number_from_string::<usize, D>(deserializer)?;

    match T::from_usize(n) {
        Some(ct) => Ok(ct),
        None => Err(serde::de::Error::custom("Invalid valid for enum")),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn serialize_control_value() {
        let json = r#"{"id":"2","value":"49"}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlType::Gain);
                assert_eq!(cv.value, "49");
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }
    }
}
