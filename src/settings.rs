use log::trace;
use protobuf::Message;
use serde::Serialize;
use std::{
    collections::HashMap,
    fmt::{self, Display},
};
use thiserror::Error;

use crate::protos::RadarMessage::RadarMessage;

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

pub struct Controls {
    pub controls: HashMap<ControlType, Control>,
    update_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
}

impl Controls {
    pub fn new(
        controls: HashMap<ControlType, Control>,
        update_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
    ) -> Self {
        Controls {
            controls,
            update_tx,
        }
    }

    fn broadcast(update_tx: &tokio::sync::broadcast::Sender<Vec<u8>>, control: &Control) {
        let mut control_value = crate::protos::RadarMessage::radar_message::ControlValue::new();
        control_value.id = control.item.control_type.to_string();
        control_value.value = control.value;
        control_value.auto = control.auto;
        if let Some(names) = &control.item.names {
            if control.value >= 0 && control.value < names.len() as i32 {
                control_value.description = Some(names[control.value as usize].to_string());
            }
        }

        let mut message = RadarMessage::new();
        message.radar = 1;
        message.controls.push(control_value);

        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");
        match update_tx.send(bytes) {
            Err(_e) => {
                trace!(
                    "Stored control value {} value {}",
                    control.item.control_type,
                    &control.value
                );
            }
            Ok(cnt) => {
                trace!(
                    "Stored control value {} value {} and sent to {} clients",
                    control.item.control_type,
                    &control.value,
                    cnt
                );
            }
        }
    }

    pub fn set_all(
        &mut self,
        control_type: &ControlType,
        value: i32,
        auto: Option<bool>,
        state: ControlState,
    ) -> Result<Option<()>, ControlError> {
        if let Some(control) = self.controls.get_mut(control_type) {
            if control.set_all(value, auto, state)?.is_some() {
                Self::broadcast(&self.update_tx, control);
                return Ok(Some(()));
            }
            Ok(None)
        } else {
            Err(ControlError::NotSupported(*control_type))
        }
    }

    /// Set a control value, and if it is changed then broadcast the control
    /// to all listeners.
    pub fn set(
        &mut self,
        control_type: &ControlType,
        value: i32,
    ) -> Result<Option<()>, ControlError> {
        if let Some(control) = self.controls.get_mut(control_type) {
            if control
                .set_all(value, None, ControlState::Manual)?
                .is_some()
            {
                Self::broadcast(&self.update_tx, control);
                return Ok(Some(()));
            }
            Ok(None)
        } else {
            Err(ControlError::NotSupported(*control_type))
        }
    }
    pub fn set_auto(
        &mut self,
        control_type: &ControlType,
        auto: bool,
        value: i32,
    ) -> Result<Option<()>, ControlError> {
        if let Some(control) = self.controls.get_mut(control_type) {
            let state = if auto {
                ControlState::Auto
            } else {
                ControlState::Manual
            };

            if control.set_all(value, Some(auto), state)?.is_some() {
                Self::broadcast(&self.update_tx, control);
                return Ok(Some(()));
            }
            Ok(None)
        } else {
            Err(ControlError::NotSupported(*control_type))
        }
    }
    pub fn set_string(
        &mut self,
        control_type: &ControlType,
        value: String,
    ) -> Result<Option<String>, ControlError> {
        if let Some(control) = self.controls.get_mut(control_type) {
            if control.set_string(value).is_some() {
                Self::broadcast(&self.update_tx, control);
                return Ok(control.value_string.clone());
            }
            Ok(None)
        } else {
            Err(ControlError::NotSupported(*control_type))
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Control {
    item: ControlValue,
    value: i32,
    value_string: Option<String>,
    auto: Option<bool>,
    pub state: ControlState,
}

impl Control {
    fn new(item: ControlValue) -> Self {
        let value = item.default_value.clone();
        Control {
            item,
            value,
            auto: None,
            state: ControlState::Off,
            value_string: None,
        }
    }

    // pub fn step(mut self, step: i32) -> Self {
    //     self.item.step_value = step;

    //     self
    // }

    pub fn read_only(mut self) -> Self {
        self.item.read_only = true;

        self
    }

    pub fn wire_scale_factor(mut self, wire_scale_factor: i32) -> Self {
        self.item.wire_scale_factor = wire_scale_factor;

        self
    }

    pub fn wire_offset(mut self, wire_offset: i32) -> Self {
        self.item.wire_offset = wire_offset;

        self
    }

    pub fn unit<S: AsRef<str>>(mut self, unit: S) -> Control {
        self.item.unit = Some(unit.as_ref().to_string());

        self
    }

    pub fn new_numeric(control_type: ControlType, min_value: i32, max_value: i32) -> Self {
        let control = Self::new(ControlValue {
            control_type,
            auto_values: 0,
            auto_names: None,
            has_off: false,
            has_auto: false,
            has_auto_adjustable: false,
            default_value: min_value,
            min_value,
            max_value,
            auto_adjust_min_value: 0,
            auto_adjust_max_value: 0,
            step_value: 1,
            wire_scale_factor: max_value,
            wire_offset: 0,
            unit: None,
            names: None,
            read_only: false,
            string_value: false,
        });
        control
    }

    pub fn new_auto(control_type: ControlType, min_value: i32, max_value: i32) -> Self {
        Self::new(ControlValue {
            control_type,
            auto_values: 0,
            auto_names: None,
            has_off: false,
            has_auto: true,
            has_auto_adjustable: false,
            default_value: min_value,
            min_value,
            max_value,
            auto_adjust_min_value: 0,
            auto_adjust_max_value: 0,
            step_value: 1,
            wire_scale_factor: max_value,
            wire_offset: 0,
            unit: None,
            names: None,
            read_only: false,
            string_value: false,
        })
    }

    pub fn new_list(control_type: ControlType, names: &[&str]) -> Self {
        Self::new(ControlValue {
            control_type,
            auto_values: 0,
            auto_names: None,
            has_off: false,
            has_auto: false,
            has_auto_adjustable: false,
            default_value: 0,
            min_value: 0,
            max_value: names.len() as i32 - 1,
            auto_adjust_min_value: 0,
            auto_adjust_max_value: 0,
            step_value: 1,
            wire_scale_factor: names.len() as i32 - 1,
            wire_offset: 0,
            unit: None,
            names: Some(names.into_iter().map(|n| n.to_string()).collect()),
            read_only: false,
            string_value: false,
        })
    }

    pub fn new_string(control_type: ControlType, read_only: bool) -> Self {
        let control = Self::new(ControlValue {
            control_type,
            auto_values: 0,
            auto_names: None,
            has_off: false,
            has_auto: false,
            has_auto_adjustable: false,
            default_value: 0,
            min_value: 0,
            max_value: 0,
            auto_adjust_min_value: 0,
            auto_adjust_max_value: 0,
            step_value: 1,
            wire_scale_factor: 0,
            wire_offset: 0,
            unit: None,
            names: None,
            read_only,
            string_value: true,
        });
        control
    }

    /// Read-only access to the definition of the control
    pub fn item(&self) -> &ControlValue {
        &self.item
    }

    // pub fn value(&self) -> i32 {
    //     self.value
    // }

    // pub fn auto(&self) -> Option<bool> {
    //     self.auto
    // }

    pub fn value_string(&self) -> String {
        if let Some(names) = &self.item.names {
            if let Some(v) = names.get(self.value as usize) {
                return v.to_string();
            }
        }
        return format!("{}", self.value);
    }

    pub fn set_all(
        &mut self,
        value: i32,
        auto: Option<bool>,
        state: ControlState,
    ) -> Result<Option<()>, ControlError> {
        let mut value = if self.item.wire_scale_factor != self.item.max_value {
            (value as i64 * self.item.max_value as i64 / self.item.wire_scale_factor as i64) as i32
        } else {
            value
        };
        if self.item.wire_offset == -1
            && value > self.item.max_value
            && value <= 2 * self.item.max_value
        {
            // debug!("{} value {} -> ", self.item.control_type, value);
            value -= 2 * self.item.max_value;
            // debug!("{} ..... {}", self.item.control_type, value);
        }

        if value < self.item.min_value {
            Err(ControlError::TooLow(
                self.item.control_type,
                value,
                self.item.min_value,
            ))
        } else if value > self.item.max_value {
            Err(ControlError::TooHigh(
                self.item.control_type,
                value,
                self.item.max_value,
            ))
        } else if auto.is_some() && !self.item.has_auto {
            Err(ControlError::NoAuto(self.item.control_type))
        } else if self.value != value || self.auto != auto {
            self.value = value;
            self.auto = auto;
            self.state = state;

            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(&mut self, value: String) -> Option<()> {
        if self.value_string.is_none() || self.value_string.as_ref().unwrap() != &value {
            self.value_string = Some(value);
            self.state = ControlState::Manual;
            Some(())
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlValue {
    control_type: ControlType,
    auto_values: i32,
    auto_names: Option<Vec<String>>,
    has_off: bool,
    has_auto: bool,
    has_auto_adjustable: bool,
    default_value: i32,
    min_value: i32,
    max_value: i32,
    auto_adjust_min_value: i32,
    auto_adjust_max_value: i32,
    step_value: i32,
    wire_scale_factor: i32,
    wire_offset: i32,
    pub unit: Option<String>,
    names: Option<Vec<String>>,
    read_only: bool,
    string_value: bool,
}

impl ControlValue {}

#[derive(Eq, PartialEq, Hash, Copy, Clone, Debug, Serialize)]
pub enum ControlType {
    AccentLight,
    // AllAuto,
    // AntennaForward,
    AntennaHeight,
    // AntennaStarboard,
    BearingAlignment,
    // ColorGain,
    // DisplayTiming,
    Doppler,
    // DopplerAutoTrack,
    DopplerSpeedThreshold,
    // Ftc,
    FirmwareVersion,
    Gain,
    InterferenceRejection,
    // LocalInterferenceRejection,
    // MainBangSize,
    // MainBangSuppression,
    Mode,
    ModelName,
    NoTransmitEnd1,
    NoTransmitEnd2,
    NoTransmitEnd3,
    NoTransmitEnd4,
    NoTransmitStart1,
    NoTransmitStart2,
    NoTransmitStart3,
    NoTransmitStart4,
    NoiseRejection,
    OperatingHours,
    // Orientation,
    Rain,
    Range,
    // Scaling,
    ScanSpeed,
    Sea,
    SeaState,
    SerialNumber,
    SideLobeSuppression,
    Status,
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
            // ControlType::LocalInterferenceRejection => "Local interference rejection",
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
