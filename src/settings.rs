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

    pub fn new_base(mut controls: HashMap<ControlType, Control>) -> Self {
        // Add controls that are not radar dependent

        let descriptions = HashMap::from([
            (0, "Off".to_string()),
            (15, "15s".to_string()),
            (30, "30s".to_string()),
            (60, "1 min".to_string()),
            (180, "3 min".to_string()),
            (300, "5 min".to_string()),
            (600, "10 min".to_string()),
        ]);

        let control = Control::new_map(ControlType::TargetTrails, descriptions);

        controls.insert(ControlType::TargetTrails, control);

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
    SetValue(ControlValue),
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlValue {
    pub fn new(id: ControlType, value: String) -> Self {
        ControlValue {
            id,
            value,
            auto: None,
            enabled: None,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Control {
    #[serde(flatten)]
    item: ControlDefinition,
    #[serde(skip)]
    pub value: Option<f32>,
    #[serde(skip)]
    pub auto_value: Option<f32>,
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(skip)]
    pub auto: Option<bool>,
    #[serde(skip)]
    pub enabled: Option<bool>,
    #[serde(skip)]
    pub needs_refresh: bool, // True when it has been changed and client needs to know value (again)
}

impl Control {
    fn new(item: ControlDefinition) -> Self {
        let value = item.default_value.clone();
        Control {
            item,
            value,
            auto_value: None,
            auto: None,
            enabled: None,
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

    pub fn wire_scale_factor(mut self, wire_scale_factor: f32, with_step: bool) -> Self {
        self.item.wire_scale_factor = Some(wire_scale_factor);
        if with_step {
            self.item.step_value =
                Some(self.item.max_value.unwrap_or(1.) / self.item.wire_scale_factor.unwrap_or(1.));
        }

        self
    }

    pub fn wire_offset(mut self, wire_offset: f32) -> Self {
        self.item.wire_offset = Some(wire_offset);

        self
    }

    pub fn unit<S: AsRef<str>>(mut self, unit: S) -> Control {
        self.item.unit = Some(unit.as_ref().to_string());

        self
    }

    pub fn send_always(mut self) -> Control {
        self.item.is_send_always = true;

        self
    }

    pub fn has_enabled(mut self) -> Self {
        self.item.has_enabled = true;

        self
    }

    pub fn new_numeric(control_type: ControlType, min_value: f32, max_value: f32) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        let control = Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: Some(1.),
            wire_scale_factor: max_value,
            wire_offset: Some(0.),
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
            is_send_always: false,
        });
        control
    }

    pub fn new_auto(
        control_type: ControlType,
        min_value: f32,
        max_value: f32,
        automatic: AutomaticValue,
    ) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: Some(automatic),
            has_enabled: false,
            default_value: min_value,
            min_value,
            max_value,
            step_value: Some(1.),
            wire_scale_factor: max_value,
            wire_offset: Some(0.),
            unit: None,
            descriptions: None,
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
            is_send_always: false,
        })
    }

    pub fn new_list(control_type: ControlType, descriptions: &[&str]) -> Self {
        let description_count = ((descriptions.len() as i32) - 1) as f32;
        Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: Some(0.),
            min_value: Some(0.),
            max_value: Some(description_count),
            step_value: Some(1.),
            wire_scale_factor: Some(description_count),
            wire_offset: Some(0.),
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
            is_send_always: false,
        })
    }

    pub fn new_map(control_type: ControlType, descriptions: HashMap<i32, String>) -> Self {
        Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_enabled: false,
            default_value: Some(0.),
            min_value: Some(0.),
            max_value: Some(((descriptions.len() as i32) - 1) as f32),
            step_value: Some(1.),
            wire_scale_factor: Some(((descriptions.len() as i32) - 1) as f32),
            wire_offset: Some(0.),
            unit: None,
            descriptions: Some(descriptions),
            valid_values: None,
            is_read_only: false,
            is_string_value: false,
            is_send_always: false,
        })
    }
    pub fn new_string(control_type: ControlType) -> Self {
        let control = Self::new(ControlDefinition {
            control_type,
            name: control_type.to_string(),
            automatic: None,
            has_enabled: false,
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
            is_send_always: false,
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
                                347 => "3/16 nm",
                                463 => "1/4 nm",
                                693 => "3/8 nm",
                                926 => "1/2 nm",
                                1157 => "5/8 nm",
                                1389 => "3/4 nm",
                                2315 => "1,25 nm",
                                2778 => "1,5 nm",
                                4630 => "2,5 nm",
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

        if self.auto.unwrap_or(false) && self.auto_value.is_some() {
            return self.auto_value.unwrap().to_string();
        }

        self.value
            .unwrap_or(self.item.default_value.unwrap_or(0.))
            .to_string()
    }

    pub fn set_auto(&mut self, auto: bool) {
        self.needs_refresh = self.auto != Some(auto);
        log::trace!(
            "Setting {} auto {} changed: {}",
            self.item.control_type,
            auto,
            self.needs_refresh
        );
        self.auto = Some(auto);
    }

    /// Set the control to a (maybe new) value + auto state
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub fn set(
        &mut self,
        mut value: f32,
        mut auto_value: Option<f32>,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        // SCALE MAPPING
        if let (Some(wire_scale_factor), Some(max_value)) =
            (self.item.wire_scale_factor, self.item.max_value)
        {
            // One of the _only_ reasons we use f32 is because Navico wire format for some things is
            // tenths of degrees. To make things uniform we map these to a float with .1 precision.
            if wire_scale_factor != max_value {
                log::trace!("{} map value {}", self.item.control_type, value);
                value = value * max_value / wire_scale_factor;

                auto_value = auto_value.map(|v| v * max_value / wire_scale_factor);
                log::trace!("{} map value to scaled {}", self.item.control_type, value);
            }
        }

        // RANGE MAPPING
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && value > max_value
                && value <= 2. * max_value
            {
                // debug!("{} value {} -> ", self.item.control_type, value);
                value -= 2. * max_value;
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

        if let Some(step) = self.item.step_value {
            match step {
                0.1 => {
                    value = (value * 10.) as i32 as f32 / 10.;
                    auto_value = auto_value.map(|value| (value * 10.) as i32 as f32 / 10.);
                }
                1.0 => {
                    value = value as i32 as f32;
                    auto_value = auto_value.map(|value| value as i32 as f32);
                }
                _ => {
                    value = (value / step).round() * step;
                    auto_value = auto_value.map(|value| (value / step).round() * step);
                }
            }
            log::trace!("{} map value to rounded {}", self.item.control_type, value);
        }

        if auto.is_some() && self.item.automatic.is_none() {
            Err(ControlError::NoAuto(self.item.control_type))
        } else if self.value != Some(value)
            || self.auto_value != auto_value
            || self.auto != auto
            || self.enabled != enabled
        {
            self.value = Some(value);
            self.auto_value = auto_value;
            self.auto = auto;
            self.enabled = enabled;
            self.needs_refresh = false;

            Ok(Some(()))
        } else if self.needs_refresh || self.item.is_send_always {
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
    //#[serde(skip)]
    //pub(crate) auto_values: i32,
    //#[serde(skip)]
    //pub(crate) auto_descriptions: Option<Vec<String>>,
    pub(crate) has_auto_adjustable: bool,
    pub(crate) auto_adjust_min_value: f32,
    pub(crate) auto_adjust_max_value: f32,
}

pub(crate) const HAS_AUTO_NOT_ADJUSTABLE: AutomaticValue = AutomaticValue {
    has_auto: true,
    has_auto_adjustable: false,
    auto_adjust_min_value: 0.,
    auto_adjust_max_value: 0.,
};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControlDefinition {
    #[serde(skip)]
    pub(crate) control_type: ControlType,
    name: String,
    //#[serde(skip)]
    //has_off: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    automatic: Option<AutomaticValue>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_enabled: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) is_string_value: bool,
    #[serde(skip)]
    default_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_value: Option<f32>,
    #[serde(skip)]
    wire_scale_factor: Option<f32>,
    #[serde(skip)]
    wire_offset: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptions: Option<HashMap<i32, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) valid_values: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "is_false")]
    is_read_only: bool,
    #[serde(skip)]
    is_send_always: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {}

#[derive(
    Eq,
    PartialEq,
    PartialOrd,
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
// The order is the one in which we deem the representation is "right"
// when present as a straight list of controls. This is the same order
// as shown in the radar page for HALO.
pub enum ControlType {
    Status,
    Range,
    Mode,
    // AllAuto,
    Gain,
    Sea,
    SeaState,
    // Stc,
    // StcCurve,
    Rain,
    // Scaling,
    Doppler,
    // DopplerAutoTrack,
    DopplerSpeedThreshold,
    // Client Only, not here: ColorPalette,
    // Client Only, not here: Orientation,
    // Client Only, not here: Position,
    // Client Only, not here: Symbology,
    TargetTrails,
    DopplerTrailsOnly,
    ClearTrails,
    // TimedIdle,
    // TimedRun,
    NoiseRejection,
    TargetBoost,
    TargetExpansion,
    InterferenceRejection,
    TargetSeparation,
    LocalInterferenceRejection,
    ScanSpeed,
    SideLobeSuppression,
    // TrailsMotion,
    // TuneCoarse,
    // TuneFine,
    // ColorGain,
    // DisplayTiming,
    Ftc,
    // MainBangSize,
    MainBangSuppression,
    NoTransmitStart1,
    NoTransmitEnd1,
    NoTransmitStart2,
    NoTransmitEnd2,
    NoTransmitStart3,
    NoTransmitEnd3,
    NoTransmitStart4,
    NoTransmitEnd4,
    AccentLight,
    // AntennaForward,
    AntennaHeight,
    // AntennaStarboard,
    BearingAlignment,
    // Orientation,
    RotationSpeed,
    OperatingHours,
    ModelName,
    FirmwareVersion,
    SerialNumber,
    UserName,
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
            ControlType::ClearTrails => "Clear trails",
            // ControlType::DisplayTiming => "Display timing",
            ControlType::Doppler => "Doppler",
            // ControlType::DopplerAutoTrack => "Doppler Auto Track",
            ControlType::DopplerSpeedThreshold => "Doppler speed threshold",
            ControlType::DopplerTrailsOnly => "Doppler trails only",
            ControlType::FirmwareVersion => "Firmware version",
            ControlType::Ftc => "FTC",
            ControlType::Gain => "Gain",
            ControlType::InterferenceRejection => "Interference rejection",
            ControlType::LocalInterferenceRejection => "Local interference rejection",
            // ControlType::MainBangSize => "Main bang size",
            ControlType::MainBangSuppression => "Main bang suppression",
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
            ControlType::RotationSpeed => "Rotation speed",
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
            ControlType::TargetTrails => "Target trails",
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
    TooLow(ControlType, f32, f32),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(ControlType, f32, f32),
    #[error("Control {0} value {1} is not a legal value")]
    Invalid(ControlType, String),
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
        let json = r#"{"id":"3","value":"49","auto":true,"enabled":false}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlType::Gain);
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, Some(true));
                assert_eq!(cv.enabled, Some(false));
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }
        let json = r#"{"id":"3","value":"49"}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlType::Gain);
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, None);
                assert_eq!(cv.enabled, None);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }
    }
}
