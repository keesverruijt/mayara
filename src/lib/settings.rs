use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Deserializer, Serialize, de::Visitor, ser::Serializer};
use serde_json::{Number, Value};
use serde_string_enum::{DeserializeLabeledStringEnum, SerializeLabeledStringEnum};
use std::cell::RefCell;
use std::f64::consts::PI;
use std::{
    borrow::Cow,
    collections::HashMap,
    convert::From,
    fmt::{self, Display},
    str::FromStr,
    sync::{Arc, RwLock},
};
use strum::{EnumCount, EnumIter, EnumString, IntoStaticStr};
use thiserror::Error;
use utoipa::ToSchema;

use crate::Cli;
use crate::{
    TargetMode,
    radar::{Power, RadarError, range::Ranges},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiVersion {
    V1 = 1,
    V3 = 3,
}

thread_local! {
    static API_VERSION: RefCell<ApiVersion> = RefCell::new(ApiVersion::V3);
}

pub fn set_api_version(version: ApiVersion) {
    API_VERSION.with(|v| {
        *v.borrow_mut() = version;
    });
}
pub fn get_api_version() -> ApiVersion {
    API_VERSION.with(|v| v.borrow().clone())
}

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
/// Per radar we keep a single Controls structure in memory that is
/// accessed from all threads that are working for that radar and any user
/// clients.
///
/// If you've got a reference to the controls object for a radar, you can
/// subscribe to any changes made to it.
///

#[derive(Debug, Serialize)]
pub struct Controls {
    #[serde(skip)]
    replay: bool,

    #[serde(flatten)]
    controls: HashMap<ControlId, Control>,

    #[serde(skip)]
    key: Option<String>, // A copy of the radar's key() value
    #[serde(skip)]
    all_clients_tx: tokio::sync::broadcast::Sender<ControlValue>,
    #[serde(skip)]
    sk_client_tx: Option<tokio::sync::broadcast::Sender<RadarControlValue>>,
    #[serde(skip)]
    control_update_tx: tokio::sync::broadcast::Sender<ControlUpdate>,
}

impl Controls {
    pub(self) fn insert(&mut self, control_id: ControlId, value: Control) {
        let v = Control {
            item: ControlDefinition {
                is_read_only: self.replay || value.item.is_read_only,
                ..value.item
            },
            ..value
        };

        self.controls.insert(control_id, v);
    }

    pub(self) fn new_base(args: &Cli, mut controls: HashMap<ControlId, Control>) -> Self {
        // Add _mandatory_ controls
        if !controls.contains_key(&ControlId::ModelName) {
            controls.insert(
                ControlId::ModelName,
                Control::new_string(ControlId::ModelName).read_only(true),
            );
        }
        controls.insert(
            ControlId::Spokes,
            Control::new_numeric(ControlId::Spokes, 0., 9999.).read_only(true),
        );

        if args.replay {
            controls.iter_mut().for_each(|(_k, v)| {
                v.item.is_read_only = true;
            });
        }

        // Add controls that are not radar dependent

        controls.insert(
            ControlId::UserName,
            Control::new_string(ControlId::UserName).read_only(false),
        );

        if args.targets != TargetMode::None {
            controls.insert(
                ControlId::TargetTrails,
                Control::new_map(
                    ControlId::TargetTrails,
                    HashMap::from([
                        (0, "Off".to_string()),
                        (1, "15s".to_string()),
                        (2, "30s".to_string()),
                        (3, "1 min".to_string()),
                        (4, "3 min".to_string()),
                        (5, "5 min".to_string()),
                        (6, "10 min".to_string()),
                    ]),
                ),
            );

            controls.insert(
                ControlId::TrailsMotion,
                Control::new_map(
                    ControlId::TrailsMotion,
                    HashMap::from([(0, "Relative".to_string()), (1, "True".to_string())]),
                ),
            );

            controls.insert(
                ControlId::ClearTrails,
                Control::new_button(ControlId::ClearTrails),
            );

            if args.targets == TargetMode::Arpa {
                controls.insert(
                    ControlId::ClearTargets,
                    Control::new_button(ControlId::ClearTargets),
                );
            }
        }

        let (all_clients_tx, _) = tokio::sync::broadcast::channel(32);
        let (control_update_tx, _) = tokio::sync::broadcast::channel(32);

        Controls {
            replay: args.replay,
            controls,
            key: None,
            all_clients_tx,
            sk_client_tx: None,
            control_update_tx,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SharedControls {
    #[serde(flatten, with = "arc_rwlock_serde")]
    controls: Arc<RwLock<Controls>>,
}

mod arc_rwlock_serde {
    use serde::de::Deserializer;
    use serde::ser::Serializer;
    use serde::{Deserialize, Serialize};
    use std::sync::{Arc, RwLock};

    pub fn serialize<S, T>(val: &Arc<RwLock<T>>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        T: Serialize,
    {
        T::serialize(&*val.read().unwrap(), s)
    }

    #[allow(dead_code)]
    pub fn deserialize<'de, D, T>(d: D) -> Result<Arc<RwLock<T>>, D::Error>
    where
        D: Deserializer<'de>,
        T: Deserialize<'de>,
    {
        Ok(Arc::new(RwLock::new(T::deserialize(d)?)))
    }
}

impl SharedControls {
    // Create a new set of controls, for a radar.
    // There is only one set that is shared amongst the various threads and
    // structs, hence the word Shared.
    pub fn new(args: &Cli, mut controls: HashMap<ControlId, Control>) -> SharedControls {
        // All radars must have the same Status control
        let mut control = Control::new_list(
            ControlId::Power,
            &["Off", "Standby", "Transmit", "Preparing"],
        )
        .send_always();
        control.set_valid_values([1, 2].to_vec()); // Only allow setting to Standby (index 1) and Transmit (index 2)
        controls.insert(ControlId::Power, control);

        SharedControls {
            controls: Arc::new(RwLock::new(Controls::new_base(args, controls))),
        }
    }

    #[deprecated]
    pub(crate) fn set_radar_info(
        &mut self,
        sk_client_tx: tokio::sync::broadcast::Sender<RadarControlValue>,
        radar_label: String,
    ) {
        let mut locked = self.controls.write().unwrap();
        locked.key = Some(radar_label);
        locked.sk_client_tx = Some(sk_client_tx);
    }

    fn get_command_tx(&self) -> tokio::sync::broadcast::Sender<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.clone()
    }

    pub fn get_controls(&self) -> HashMap<ControlId, Control> {
        let locked = self.controls.read().unwrap();

        locked.controls.clone()
    }

    pub(crate) fn new_client_subscription(&self) -> tokio::sync::broadcast::Receiver<ControlValue> {
        let locked = self.controls.read().unwrap();

        locked.all_clients_tx.subscribe()
    }

    // process_client_request()
    //
    // In theory this could be from anywhere that somebody holds a SharedControls reference,
    // but in practice only called from the websocket request handler in web.rs.
    // The end user has sent a control update and we need to process this.
    //
    // Some controls are handled internally, some in the data handler for a radar and the
    // rest are settings that need to be sent to the radar.
    //
    pub fn process_client_request(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let control = self.get(&control_value.id);

        match control {
            Some(c) => {
                log::debug!(
                    "Client request to update {:?} to {:?}",
                    ControlValue::from(&c, None),
                    control_value
                );
                match control_value.id.get_destination() {
                    ControlDestination::Internal => self
                        .set_value(&control_value.id, control_value.value.clone())
                        .map(|_| ())
                        .map_err(|e| RadarError::ControlError(e)),
                    ControlDestination::Command
                    | ControlDestination::Target
                    | ControlDestination::Trail => {
                        self.send_to_command_handler(&c, control_value.clone(), reply_tx)
                    }
                    ControlDestination::ReadOnly => {
                        Err(RadarError::CannotSetControlId(control_value.id))
                    }
                }
            }
            None => Err(RadarError::CannotSetControlId(control_value.id)),
        }
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.subscribe()
    }

    pub fn get_radar_control_values(&self) -> Vec<RadarControlValue> {
        let locked = self.controls.read().unwrap();

        locked
            .controls
            .iter()
            .map(|(_, c)| RadarControlValue::new(locked.key.as_ref().unwrap(), c, None))
            .collect()
    }

    pub async fn send_all_controls(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let controls: Vec<Control> = {
            let locked = self.controls.read().unwrap();

            locked.controls.clone().into_values().collect()
        };

        for c in controls {
            self.send_reply_to_client(reply_tx.clone(), &c, None)
                .await?;
        }
        Ok(())
    }

    fn send_to_command_handler(
        &self,
        control: &Control,
        mut control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        if let Some(units) = control_value.units {
            let value = control_value.value.as_f64();
            if value.is_none() {
                return Err(RadarError::NotNumeric(
                    control_value.id,
                    control_value.value,
                    units,
                ));
            }

            let (mut units, mut value) = units.to_si(value.unwrap());
            if let Some(wire_unit) = control.item.wire_unit {
                value = wire_unit.from_si(units, value);
                units = wire_unit;
            }
            let value = serde_json::Number::from_f64(value);
            if value.is_none() {
                return Err(RadarError::NotNumeric(
                    control_value.id,
                    control_value.value,
                    units,
                ));
            }
            control_value.units = Some(units);
            control_value.value = Value::Number(value.unwrap());
        }
        let control_update = ControlUpdate {
            control_value,
            reply_tx,
        };
        self.get_command_tx()
            .send(control_update)
            .map(|_| ())
            .map_err(|_| RadarError::Shutdown)
    }

    fn send_to_all_clients(&self, control: &Control) {
        let control_value = ControlValue::from(control, None);
        let locked = self.controls.read().unwrap();
        match locked.all_clients_tx.send(control_value.clone()) {
            Err(_e) => {}
            Ok(cnt) => {
                log::trace!(
                    "Sent control value {} to {} JSON clients",
                    control.item().control_id,
                    cnt
                );
            }
        }
        if let (Some(label), Some(sk_client_tx)) = (&locked.key, &locked.sk_client_tx) {
            let radar_control_value = RadarControlValue::new(label, control, None);

            match sk_client_tx.send(radar_control_value) {
                Err(_e) => {}
                Ok(cnt) => {
                    log::trace!(
                        "Sent control value {} to {} SK clients",
                        control.item().control_id,
                        cnt
                    );
                }
            }
        }
    }

    pub async fn send_reply_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        control: &Control,
        error: Option<String>,
    ) -> Result<(), RadarError> {
        let control_value = ControlValue::from(control, error);

        log::debug!(
            "Sending reply {:?} to requesting JSON client",
            &control_value,
        );

        reply_tx
            .send(control_value)
            .await
            .map_err(|_| RadarError::Shutdown)
    }

    pub async fn send_error_to_client(
        &self,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
        cv: &ControlValue,
        e: &RadarError,
    ) -> Result<(), RadarError> {
        if let Some(control) = self.get(&cv.id) {
            self.send_reply_to_client(reply_tx, &control, Some(e.to_string()))
                .await?;
            log::warn!("User tried to set invalid {}: {}", cv.id, e);
            Ok(())
        } else {
            Err(RadarError::CannotSetControlId(cv.id))
        }
    }

    // ******* GET & SET METHODS

    pub fn insert(&self, control_id: ControlId, value: Control) {
        let mut locked = self.controls.write().unwrap();

        locked.insert(control_id, value);
    }

    pub fn get(&self, control_id: &ControlId) -> Option<Control> {
        let locked = self.controls.read().unwrap();

        locked.controls.get(control_id).cloned()
    }

    pub fn get_by_id(&self, control_id: &str) -> Option<Control> {
        match ControlId::parse_str(Cow::Borrowed(control_id)) {
            Ok(cv) => self.get(&cv),
            Err(_) => None,
        }
    }

    pub fn get_control_keys(&self) -> Vec<&'static str> {
        let locked = self.controls.read().unwrap();

        locked.controls.iter().map(|(k, _)| k.into()).collect()
    }

    pub fn contains_key(&self, control_id: &ControlId) -> bool {
        let locked = self.controls.read().unwrap();

        locked.controls.contains_key(control_id)
    }

    pub fn set_refresh(&self, control_id: &ControlId) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.needs_refresh = true;
        }
    }

    pub fn set_allowed(&self, control_id: &ControlId, read_only: bool) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.set_allowed(read_only);
        }
    }

    pub fn set_value_auto_enabled<T>(
        &self,
        control_id: &ControlId,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError>
    where
        f64: From<T>,
    {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set(value.into(), None, auto, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_wire_range(
        &self,
        control_id: &ControlId,
        min: f64,
        max: f64,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control.set_wire_range(min, max)?.map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    //
    // Set a control from a wire value, so apply all transformations
    // to convert it to a user visible value
    //
    pub fn set(
        &self,
        control_id: &ControlId,
        value: f64,
        auto: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                Ok(control
                    .set(value, None, auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_auto_state(&self, control_id: &ControlId, auto: bool) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_id) {
            control.set_auto(auto);
        } else {
            return Err(ControlError::NotSupported(*control_id));
        };
        Ok(())
    }

    pub fn set_value_auto(
        &self,
        control_id: &ControlId,
        auto: bool,
        value: f64,
    ) -> Result<Option<()>, ControlError> {
        self.set(control_id, value, Some(auto))
    }

    pub fn set_value_with_many_auto(
        &self,
        control_id: &ControlId,
        value: f64,
        auto_value: f64,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                let auto = control.auto;
                Ok(control
                    .set(value, Some(auto_value), auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        // If the control changed, control.set returned Some(control)
        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(
        &self,
        control_id: &ControlId,
        value: String,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                if control.item().data_type == ControlDataType::String {
                    Ok(control.set_string(value).map(|_| control.clone()))
                } else {
                    let i = value
                        .parse::<i32>()
                        .map_err(|_| ControlError::Invalid(control_id.clone(), value))?;
                    control
                        .set(i as f64, None, None, None)
                        .map(|_| Some(control.clone()))
                }
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    pub fn set_value(
        &self,
        control_id: &ControlId,
        value: Value,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_id) {
                if control.item().data_type == ControlDataType::String {
                    match value {
                        Value::String(s) => Ok(control.set_string(s).map(|_| control.clone())),
                        _ => Err(ControlError::Invalid(*control_id, format!("{:?}", value))),
                    }
                } else {
                    match value.clone() {
                        Value::String(s) => {
                            let i = s.parse::<i32>().map_err(|_| {
                                ControlError::Invalid(control_id.clone(), format!("{:?}", value))
                            })?;
                            control
                                .set(i as f64, None, None, None)
                                .map(|_| Some(control.clone()))
                        }
                        Value::Bool(b) => {
                            let i = b as i32 as f64;
                            control
                                .set(i as f64, None, None, None)
                                .map(|_| Some(control.clone()))
                        }
                        Value::Number(n) => match n.as_f64() {
                            Some(n) => control
                                .set(n as f64, None, None, None)
                                .map(|_| Some(control.clone())),
                            None => Err(ControlError::Invalid(
                                control_id.clone(),
                                format!("{:?}", value),
                            )),
                        },
                        _ => Err(ControlError::Invalid(
                            control_id.clone(),
                            format!("{:?}", value),
                        )),
                    }
                }
            } else {
                Err(ControlError::NotSupported(*control_id))
            }
        }?;

        if let Some(control) = control {
            self.send_to_all_clients(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)]
    fn get_description(control: &Control) -> Option<String> {
        if let (Some(value), Some(descriptions)) = (control.value, &control.item().descriptions) {
            let value = value as i32;
            if value >= 0 && value < (descriptions.len() as i32) {
                return descriptions.get(&value).cloned();
            }
        }
        return None;
    }

    pub fn set_user_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut(&ControlId::UserName).unwrap();
        control.set_string(name);
    }

    pub fn user_name(&self) -> String {
        self.get(&ControlId::UserName)
            .and_then(|c| c.description)
            .unwrap()
    }

    pub fn set_model_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut(&ControlId::ModelName).unwrap();
        control.set_string(name.clone());
    }

    pub fn model_name(&self) -> Option<String> {
        self.get(&ControlId::ModelName).and_then(|c| c.description)
    }

    pub fn set_valid_values(
        &self,
        control_id: &ControlId,
        valid_values: Vec<i32>,
    ) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(control_id)
            .ok_or(ControlError::NotSupported(*control_id))
            .map(|c| {
                c.set_valid_values(valid_values);
                ()
            })
    }

    pub fn set_valid_ranges(
        &self,
        control_id: &ControlId,
        ranges: &Ranges,
    ) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(control_id)
            .ok_or(ControlError::NotSupported(*control_id))
            .map(|c| {
                c.set_valid_ranges(ranges);
                ()
            })?;
        Ok(())
    }

    pub(crate) fn get_status(&self) -> Option<Power> {
        let locked = self.controls.read().unwrap();
        if let Some(control) = locked.controls.get(&ControlId::Power) {
            return Power::from_value(&control.value()).ok();
        }

        None
    }
}

#[derive(Clone, Debug)]
pub struct ControlUpdate {
    pub reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    pub control_value: ControlValue,
}

#[derive(
    Copy,
    PartialEq,
    SerializeLabeledStringEnum,
    DeserializeLabeledStringEnum,
    Clone,
    Debug,
    ToSchema,
)]
pub enum Units {
    #[string = ""]
    None,
    #[string = "m"]
    Meters,
    #[string = "km"]
    KiloMeters,
    #[string = "nm"]
    NauticalMiles,
    #[string = "m/s"]
    MetersPerSecond,
    #[string = "kn"]
    Knots,
    #[string = "deg"]
    Degrees,
    #[string = "rad"]
    Radians,
    #[string = "rad/s"]
    RadiansPerSecond,
    #[string = "rpm"]
    RotationsPerMinute,
    #[string = "s"]
    Seconds,
    #[string = "min"]
    Minutes,
    #[string = "h"]
    Hours,
}

const TO_SI_CONVERSIONS: [(Units, Units, f64); 7] = [
    (Units::NauticalMiles, Units::Meters, 1852.),
    (Units::KiloMeters, Units::Meters, 1000.),
    (Units::Knots, Units::MetersPerSecond, 1852. / 3600.),
    (Units::Degrees, Units::Radians, PI / 180.),
    (
        Units::RotationsPerMinute,
        Units::RadiansPerSecond,
        2. * PI / 60.,
    ),
    (Units::Minutes, Units::Seconds, 60.),
    (Units::Hours, Units::Seconds, 3600.),
];

impl Units {
    pub(crate) fn to_si(&self, value: f64) -> (Units, f64) {
        for (from, to, factor) in TO_SI_CONVERSIONS {
            if *self == from {
                return (to, value * factor);
            }
        }
        (*self, value)
    }
    pub(crate) fn from_si(&self, origin: Units, value: f64) -> f64 {
        for (from, to, factor) in TO_SI_CONVERSIONS {
            if *self == from && origin == to {
                return value / factor;
            }
        }
        value
    }
}

// This is what we send back and forth internally between (web) clients and radar managers for v1
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    pub id: ControlId,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlValue {
    pub(crate) fn new(id: ControlId, value: Value) -> Self {
        ControlValue {
            id,
            value,
            units: None,
            auto: None,
            enabled: None,
            allowed: None,
            error: None,
        }
    }

    pub fn from_request(
        id: ControlId,
        value: Value,
        auto: Option<bool>,
        units: Option<Units>,
    ) -> Self {
        ControlValue {
            id,
            value,
            units,
            auto,
            enabled: None,
            allowed: None,
            error: None,
        }
    }

    pub fn from(control: &Control, error: Option<String>) -> Self {
        ControlValue {
            id: control.item().control_id,
            value: control.value(),
            units: control.item().unit.clone(),
            auto: control.auto,
            enabled: control.enabled,
            allowed: control.allowed,
            error,
        }
    }

    pub fn as_bool(&self) -> Result<bool, RadarError> {
        self.as_i64().map(|n| n != 0)
    }

    pub fn as_i32(&self) -> Result<i32, RadarError> {
        self.as_i64()?
            .try_into()
            .map_err(|_| RadarError::CannotSetControlIdValue(self.id, self.value.clone()))
    }

    pub fn as_i64(&self) -> Result<i64, RadarError> {
        match self.value.clone() {
            Value::String(s) => {
                // TODO enum style
                s.parse::<i64>().map_err(|_| RadarError::EnumerationFailed)
            }
            Value::Bool(b) => Ok(if b { 1 } else { 0 }),
            Value::Number(n) => n.as_i64().ok_or(RadarError::EnumerationFailed),
            _ => Err(RadarError::EnumerationFailed),
        }
        .map_err(|_| RadarError::CannotSetControlIdValue(self.id, self.value.clone()))
    }

    pub fn as_f32(&self) -> Result<f64, RadarError> {
        self.as_f64().map(|n| n as f64)
    }

    pub fn as_f64(&self) -> Result<f64, RadarError> {
        match self.value.clone() {
            Value::String(s) => {
                // TODO enum style
                s.parse::<f64>().map_err(|_| RadarError::EnumerationFailed)
            }
            Value::Bool(b) => Ok(if b { 1. } else { 0. }),
            Value::Number(n) => n.as_f64().ok_or(RadarError::EnumerationFailed),
            _ => Err(RadarError::EnumerationFailed),
        }
        .map_err(|_| RadarError::CannotSetControlIdValue(self.id, self.value.clone()))
    }
}

// This is the represenation of a control value used by the Signal K (web) services
// It is the same as the V1 ControlValue but it contains a path instead of just a
// control Id.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RadarControlValue {
    #[serde(skip)]
    pub radar_id: Option<String>,
    #[serde(skip)]
    pub control_id: Option<ControlId>,
    pub path: String, // "radars.{id}.{control_id}"
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RadarControlValue {
    pub fn new(radar: &str, control: &Control, error: Option<String>) -> Self {
        RadarControlValue {
            path: format!("radars.{}.{}", radar, control.item().control_id),
            radar_id: Some(radar.to_string()),
            control_id: Some(control.item().control_id),
            value: control.value(),
            units: control.item().unit.clone(),
            auto: control.auto,
            enabled: control.enabled,
            allowed: control.allowed,
            error,
        }
    }

    pub fn parse_path(&mut self) -> Option<&str> {
        let mut path = self.path.as_str();
        if path.starts_with("radars.") {
            path = &path["radars.".len()..];
        }
        if let Some(r) = path.split_once('.') {
            self.control_id = ControlId::try_from(r.1).ok();
            if self.control_id.is_some() {
                self.radar_id = Some(r.0.to_string());
                return Some(r.0);
            }
        }

        None
    }
}

impl From<RadarControlValue> for ControlValue {
    fn from(value: RadarControlValue) -> Self {
        ControlValue {
            id: value.control_id.unwrap(),
            value: value.value,
            units: value.units,
            auto: value.auto,
            enabled: value.enabled,
            allowed: value.allowed,
            error: value.error,
        }
    }
}

// This is the represenation of a control value used by the Signal K REST API
#[derive(Deserialize, Serialize, Clone, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FullRadarControlValue {
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub units: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<RadarControlValue> for FullRadarControlValue {
    fn from(value: RadarControlValue) -> Self {
        FullRadarControlValue {
            value: value.value,
            units: value.units,
            auto: value.auto,
            enabled: value.enabled,
            allowed: value.allowed,
            error: value.error,
        }
    }
}

impl From<ControlValue> for FullRadarControlValue {
    fn from(value: ControlValue) -> Self {
        FullRadarControlValue {
            value: value.value,
            units: value.units,
            auto: value.auto,
            enabled: value.enabled,
            allowed: value.allowed,
            error: value.error,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
    #[serde(flatten)]
    item: ControlDefinition,
    #[serde(skip)]
    pub value: Option<f64>,
    #[serde(skip)]
    pub auto_value: Option<f64>,
    #[serde(skip)]
    pub description: Option<String>,
    #[serde(skip)]
    pub auto: Option<bool>,
    #[serde(skip)]
    pub enabled: Option<bool>,
    #[serde(skip)]
    pub allowed: Option<bool>,
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
            allowed: None,
            needs_refresh: false,
        }
    }

    pub(crate) fn read_only(mut self, is_read_only: bool) -> Self {
        self.item.is_read_only = is_read_only;

        self
    }

    pub fn set_allowed(&mut self, allowed: bool) {
        if self.allowed != Some(allowed) {
            self.allowed = Some(allowed);
            self.needs_refresh = true;
        }
    }

    pub(crate) fn wire_scale_step(mut self, step: f64) -> Self {
        self.item.step_value = Some(step);
        if self.item.wire_scale_factor.is_none() {
            self.item.wire_scale_factor = self.item.step_value.map(|s| 1. / s);
        }

        self
    }

    pub(crate) fn wire_scale_factor(mut self, wire_scale_factor: f64, with_step: bool) -> Self {
        self.item.wire_scale_factor = Some(wire_scale_factor);
        if with_step {
            self.item.step_value = self.item.wire_scale_factor.map(|f| 1. / f);
        }

        self
    }

    pub(crate) fn wire_offset(mut self, wire_offset: f64) -> Self {
        self.item.wire_offset = Some(wire_offset);

        self
    }

    pub(crate) fn wire_unit(mut self, unit: Units) -> Control {
        self.item.wire_unit = Some(unit);
        self.item.unit = Some(unit.to_si(0.).0);

        self
    }

    pub(crate) fn send_always(mut self) -> Control {
        self.item.is_send_always = true;

        self
    }

    pub(crate) fn has_enabled(mut self) -> Self {
        self.item.has_enabled = true;

        self
    }

    pub(crate) fn new_numeric(control_id: ControlId, min_value: f64, max_value: f64) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        let control = Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::Number,
            min_value,
            None,
            false,
            min_value,
            max_value,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            false,
        ));
        control
    }

    pub(crate) fn new_auto(
        control_id: ControlId,
        min_value: f64,
        max_value: f64,
        automatic: AutomaticValue,
    ) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::Number,
            min_value,
            Some(automatic),
            false,
            min_value,
            max_value,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            false,
        ))
    }

    pub(crate) fn new_list(control_id: ControlId, descriptions: &[&str]) -> Self {
        let description_count = ((descriptions.len() as i32) - 1) as f64;
        Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::Number,
            Some(0.),
            None,
            false,
            Some(0.),
            Some(description_count),
            None,
            None,
            None,
            None,
            Some(
                descriptions
                    .into_iter()
                    .enumerate()
                    .map(|(i, n)| (i as i32, n.to_string()))
                    .collect(),
            ),
            None,
            false,
            false,
        ))
    }

    pub(crate) fn new_map(control_id: ControlId, descriptions: HashMap<i32, String>) -> Self {
        Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::Number,
            Some(0.),
            None,
            false,
            Some(0.),
            Some(((descriptions.len() as i32) - 1) as f64),
            None,
            None,
            None,
            None,
            Some(descriptions),
            None,
            false,
            false,
        ))
    }

    pub(crate) fn new_string(control_id: ControlId) -> Self {
        Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::String,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            true,
            false,
        ))
    }

    pub(crate) fn new_button(control_id: ControlId) -> Self {
        Self::new(ControlDefinition::new(
            control_id,
            ControlDataType::Button,
            None,
            None,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            false,
        ))
    }

    /// Read-only access to the definition of the control
    pub fn item(&self) -> &ControlDefinition {
        &self.item
    }

    pub(crate) fn set_valid_values(&mut self, values: Vec<i32>) {
        self.item.valid_values = Some(values);
    }

    pub(crate) fn set_valid_ranges(&mut self, ranges: &Ranges) {
        let mut values = Vec::new();
        let mut descriptions = HashMap::new();
        for range in ranges.all.iter() {
            values.push(range.distance());
            descriptions.insert(range.distance() as i32, format!("{}", range));
        }

        self.item.valid_values = Some(values);
        self.item.descriptions = Some(descriptions);
    }

    // pub fn auto(&self) -> Option<bool> {
    //     self.auto
    // }

    fn to_number(&self, v: f64) -> Number {
        if let Some(n) = {
            if v == v as i32 as f64 {
                Number::from_i128(v as i128)
            } else {
                Number::from_f64(v as f64)
            }
        } {
            n
        } else {
            panic!("Cannot reprsent {:?} as number", self);
        }
    }

    pub fn value(&self) -> Value {
        if self.item.data_type == ControlDataType::String {
            return Value::String(self.description.clone().unwrap_or_else(|| "".to_string()));
        }

        if self.auto.unwrap_or(false) && self.auto_value.is_some() {
            return Value::Number(self.to_number(self.auto_value.unwrap()));
        }

        let v: f64 = self.value.unwrap_or(self.item.default_value.unwrap_or(0.));

        Value::Number(self.to_number(v))
    }

    pub fn as_f32(&self) -> Option<f64> {
        self.value
    }

    pub fn as_u16(&self) -> Option<u16> {
        match self.value {
            None => None,
            Some(n) => n.to_u16(),
        }
    }

    pub fn set_auto(&mut self, auto: bool) {
        self.needs_refresh = self.auto != Some(auto);
        log::trace!(
            "Setting {} auto {} changed: {}",
            self.item.control_id,
            auto,
            self.needs_refresh
        );
        self.auto = Some(auto);
    }

    ///
    /// Set a control from a wire value, so apply all transformations
    /// to convert it to a user visible value
    ///
    /// Set the control to a (maybe new) value + auto state
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub fn set(
        &mut self,
        mut value: f64,
        mut auto_value: Option<f64>,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        // SCALE MAPPING
        log::trace!(
            "{}: set(value={},auto_value={:?},auto={:?},enabled={:?}) with item {:?}",
            self.item.name,
            value,
            auto_value,
            auto,
            enabled,
            self.item
        );

        if let Some(wire_offset) = self.item.wire_offset {
            if wire_offset > 0.0 {
                value -= wire_offset;
            }
        }
        if let Some(wire_scale_factor) = self.item.wire_scale_factor {
            // One of the reasons we use f64 is because Navico wire format for some things is
            // tenths of degrees. To make things uniform we map these to a float with .1 precision.

            log::trace!("{} map value {}", self.item.control_id, value);
            value = value / wire_scale_factor;

            auto_value = auto_value.map(|v| v / wire_scale_factor);
            log::trace!("{} map value to scaled {}", self.item.control_id, value);
        }

        // RANGE MAPPING
        if let (Some(min_value), Some(max_value)) = (self.item.min_value, self.item.max_value) {
            if self.item.wire_offset.unwrap_or(0.) == -1.
                && value > max_value
                && value <= 2. * max_value
            {
                // debug!("{} value {} -> ", self.item.control_id, value);
                value -= 2. * max_value;
                // debug!("{} ..... {}", self.item.control_id, value);
            }

            if value < min_value {
                return Err(ControlError::TooLow(self.item.control_id, value, min_value));
            }
            if value > max_value {
                return Err(ControlError::TooHigh(
                    self.item.control_id,
                    value,
                    max_value,
                ));
            }
        }

        let step = self.item.step_value.unwrap_or(1.0);
        match step {
            0.1 => {
                value = (value * 10.) as i32 as f64 / 10.;
                auto_value = auto_value.map(|value| (value * 10.) as i32 as f64 / 10.);
            }
            1.0 => {
                value = value as i32 as f64;
                auto_value = auto_value.map(|value| value as i32 as f64);
            }
            _ => {
                value = (value / step).round() * step;
                auto_value = auto_value.map(|value| (value / step).round() * step);
            }
        }
        log::trace!("{} map value to rounded {}", self.item.control_id, value);

        let wire_value = value;
        let value = self
            .item
            .wire_unit
            .map(|u| u.to_si(value).1)
            .unwrap_or(value);
        log::info!(
            "value {} {} -> {} {}",
            wire_value,
            self.item.wire_unit.unwrap_or(Units::None),
            value,
            self.item.unit.unwrap_or(Units::None)
        );
        if let Some(av) = auto_value {
            let si = self
                .item
                .wire_unit
                .map(|u| u.to_si(value).1)
                .unwrap_or(value);
            log::info!(
                "auto value {} {} -> {} {}",
                av,
                self.item.wire_unit.unwrap_or(Units::None),
                si,
                self.item.unit.unwrap_or(Units::None)
            );
            auto_value = Some(si);
        }

        if auto.is_some() && self.item.automatic.is_none() {
            Err(ControlError::NoAuto(self.item.control_id))
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
            log::trace!("Set {} to {:?}", self.item.control_id, self.description);
            Some(())
        } else if self.needs_refresh {
            self.needs_refresh = false;
            Some(())
        } else {
            None
        }
    }

    /// Set the control's wire offset and scale
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub(crate) fn set_wire_range(
        &mut self,
        min: f64,
        max: f64,
    ) -> Result<Option<()>, ControlError> {
        let max = Some(max - min);
        let min = if min != 0.0 { Some(min) } else { None };

        if min != self.item.wire_offset || max != self.item.wire_scale_factor {
            log::debug!(
                "{}: new wire offset {:?} and scale {:?}",
                self.item.name,
                min,
                max,
            );
            self.item.wire_offset = min;
            self.item.wire_scale_factor = max;
        }
        Ok(None)
    }

    /// Look up the wire value (index) for an enum value by its string value or label
    /// Returns None if no match found or if not an enum control
    #[allow(dead_code)]
    pub(crate) fn enum_value_to_index(&self, value_str: &str) -> Option<usize> {
        if let Some(descriptions) = &self.item.descriptions {
            for (idx, label) in descriptions.iter() {
                if label.eq_ignore_ascii_case(value_str) {
                    return Some(*idx as usize);
                }
            }
        }
        None
    }

    /// Look up the string value for an enum by its index
    /// Returns the core definition's value if available, otherwise the label
    #[allow(dead_code)]
    pub(crate) fn index_to_enum_value(&self, index: usize) -> Option<String> {
        if let Some(descriptions) = &self.item.descriptions {
            return descriptions.get(&(index as i32)).cloned();
        }
        None
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticValue {
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_auto: bool,
    //#[serde(skip)]
    //pub(crate) auto_values: i32,
    //#[serde(skip)]
    //pub(crate) auto_descriptions: Option<Vec<String>>,
    pub(crate) has_auto_adjustable: bool,
    pub(crate) auto_adjust_min_value: f64,
    pub(crate) auto_adjust_max_value: f64,
}

pub(crate) const HAS_AUTO_NOT_ADJUSTABLE: AutomaticValue = AutomaticValue {
    has_auto: true,
    has_auto_adjustable: false,
    auto_adjust_min_value: 0.,
    auto_adjust_max_value: 0.,
};

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ControlDataType {
    Number,
    String,
    Button,
}

#[derive(Clone, Debug)]
pub(crate) enum ControlDestination {
    ReadOnly,
    Internal,
    Command,
    Trail,
    Target,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlDefinition {
    pub(crate) id: u8,
    #[serde(skip)]
    pub control_id: ControlId,
    name: &'static str,
    description: &'static str,
    category: Category,
    pub(crate) data_type: ControlDataType,
    //#[serde(skip)]
    //has_off: bool,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    automatic: Option<AutomaticValue>,
    #[serde(skip_serializing_if = "is_false")]
    pub(crate) has_enabled: bool,
    #[serde(skip)]
    default_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) min_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_value: Option<f64>,
    #[serde(skip)]
    wire_scale_factor: Option<f64>,
    #[serde(skip)]
    wire_offset: Option<f64>,
    #[serde(skip)]
    pub(crate) wire_unit: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) unit: Option<Units>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) descriptions: Option<HashMap<i32, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) valid_values: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "is_false")]
    is_read_only: bool,
    #[serde(skip)]
    is_send_always: bool, // Whether the controlvalue is sent out to client in all state messages
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {
    fn new(
        control_id: ControlId,
        data_type: ControlDataType,
        default_value: Option<f64>,
        automatic: Option<AutomaticValue>,
        has_enabled: bool,
        min_value: Option<f64>,
        max_value: Option<f64>,
        step_value: Option<f64>,
        wire_scale_factor: Option<f64>,
        wire_offset: Option<f64>,
        wire_unit: Option<Units>,
        descriptions: Option<HashMap<i32, String>>,
        valid_values: Option<Vec<i32>>,
        is_read_only: bool,
        is_send_always: bool,
    ) -> Self {
        let step_value = if data_type == ControlDataType::Number {
            step_value.or(Some(1.0))
        } else {
            step_value
        };

        let unit = wire_unit.map(|u| u.to_si(0.0).0);

        ControlDefinition {
            id: control_id as u8,
            control_id,
            name: control_id.get_name(),
            description: control_id.get_description(),
            category: control_id.get_category(),
            data_type,
            automatic,
            has_enabled,
            default_value,
            min_value,
            max_value,
            step_value,
            wire_scale_factor,
            wire_offset,
            unit,
            wire_unit,
            descriptions,
            valid_values,
            is_read_only,
            is_send_always,
        }
    }
}

#[derive(
    Eq,
    PartialEq,
    PartialOrd,
    Hash,
    Copy,
    Clone,
    Debug,
    FromPrimitive,
    ToPrimitive,
    EnumIter,
    EnumString,
    IntoStaticStr,
    EnumCount,
)]
#[repr(u8)]
#[strum(ascii_case_insensitive, serialize_all = "camelCase")]
// The order is the one in which we deem the representation is "right"
// when present as a straight list of controls. This is the same order
// as shown in the radar page for HALO on NOS MFDs.
pub enum ControlId {
    Power,
    WarmupTime,
    Range,
    Mode,
    // AllAuto,
    Gain,
    ColorGain,
    Sea,
    SeaState,
    // Stc,
    // StcCurve,
    Rain,
    // Scaling,
    Doppler,
    DopplerMode,
    DopplerAutoTrack,
    DopplerSpeedThreshold,
    // Client Only, not here: ColorPalette,
    // Client Only, not here: Orientation,
    // Client Only, not here: Position,
    // Client Only, not here: Symbology,
    TargetTrails,
    TrailsMotion,
    DopplerTrailsOnly,
    ClearTrails,
    ClearTargets,
    // TimedIdle,
    // TimedRun,
    NoiseRejection,
    TargetBoost,
    TargetExpansion,
    InterferenceRejection,
    TargetSeparation,
    LocalInterferenceRejection,
    BirdMode,
    ScanSpeed,
    SideLobeSuppression,
    Tune,
    // TuneCoarse,
    // TuneFine,
    // ColorGain,
    // DisplayTiming,
    Ftc,
    // MainBangSize,
    MainBangSuppression,
    SeaClutterCurve,
    DisplayTiming,
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
    MagnetronCurrent,
    SignalStrength,
    OperatingTime,
    TransmitTime,
    ModelName,
    FirmwareVersion,
    SerialNumber,
    Spokes,
    UserName,
}

impl Display for ControlId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &'static str = self.into();

        write!(f, "{}", s)
    }
}

impl ControlId {
    pub fn from_u8(value: u8) -> ControlId {
        FromPrimitive::from_u8(value).unwrap()
    }

    pub fn parse_str(s: Cow<'_, str>) -> Result<ControlId, RadarError> {
        // Numeric discriminant encoded as string
        if let Ok(num) = s.parse::<u8>() {
            return match FromPrimitive::from_u8(num) {
                Some(ct) => Ok(ct),
                None => Err(RadarError::InvalidControlId(
                    "invalid ControlId discriminant".to_string(),
                )),
            };
        }

        // Case-insensitive name lookup (Strum)
        ControlId::from_str(&s)
            .map_err(|_| RadarError::InvalidControlId("invalid ControlId discriminant".to_string()))
    }

    pub fn get_category(&self) -> Category {
        match self {
            ControlId::Power
            | ControlId::BirdMode
            | ControlId::Range
            | ControlId::Mode
            | ControlId::Gain
            | ControlId::ColorGain
            | ControlId::Sea
            | ControlId::SeaState
            | ControlId::Rain
            | ControlId::Doppler
            | ControlId::DopplerMode
            | ControlId::TargetTrails
            | ControlId::ClearTargets
            | ControlId::ClearTrails => Category::Base,
            ControlId::AccentLight
            | ControlId::AntennaHeight
            | ControlId::BearingAlignment
            | ControlId::NoTransmitEnd1
            | ControlId::NoTransmitEnd2
            | ControlId::NoTransmitEnd3
            | ControlId::NoTransmitEnd4
            | ControlId::NoTransmitStart1
            | ControlId::NoTransmitStart2
            | ControlId::NoTransmitStart3
            | ControlId::NoTransmitStart4 => Category::Installation,
            ControlId::ModelName
            | ControlId::WarmupTime
            | ControlId::FirmwareVersion
            | ControlId::OperatingTime
            | ControlId::TransmitTime
            | ControlId::MagnetronCurrent
            | ControlId::RotationSpeed
            | ControlId::SerialNumber
            | ControlId::SignalStrength => Category::Info,
            _ => Category::Advanced,
        }
    }

    pub fn get_description(&self) -> &'static str {
        match self {
            ControlId::AccentLight => "Strength of the accent light",
            ControlId::AntennaHeight => "Height of the antenna above waterline",
            ControlId::BearingAlignment => "Alignment of the antenna relative to the vessel's bow",
            ControlId::ClearTargets => "Clear all ARPA targets",
            ControlId::ClearTrails => "Clear target trails",
            ControlId::ColorGain => "Adjust the color curve relative to gain",
            ControlId::DisplayTiming => "Display timing",
            ControlId::Doppler => {
                "Targets coming towards or going away from own ship shown in different colors"
            }
            ControlId::DopplerMode => "For what type of targets Doppler is used",
            ControlId::DopplerAutoTrack => {
                "Convert all Doppler targets to ARPA targets automatically"
            }
            ControlId::DopplerSpeedThreshold => "Threshold speed above which Doppler is applied",
            ControlId::DopplerTrailsOnly => "Convert only Doppler targets to target trails",
            ControlId::FirmwareVersion => "Version of the radar firmware",
            ControlId::Ftc => "FTC",
            ControlId::Gain => "How sensitive the radar is to returning echoes",
            ControlId::InterferenceRejection => "Reduces interference from other radars",
            ControlId::LocalInterferenceRejection => {
                "How much local interference rejection is applied"
            }
            ControlId::BirdMode => "Level of optimization for bird targets",
            ControlId::MagnetronCurrent => "The current supplied to the magnetron",
            ControlId::MainBangSuppression => "Main bang suppression",
            ControlId::Mode => "Choice of radar mode tuning to certain conditions, or custom",
            ControlId::ModelName => "Manufacturer model name of the radar",
            ControlId::NoTransmitEnd1 => "End angle of the (first) no-transmit sector",
            ControlId::NoTransmitEnd2 => "End angle of the second no-transmit sector",
            ControlId::NoTransmitEnd3 => "End angle of the third no-transmit sector",
            ControlId::NoTransmitEnd4 => "End angle of the fourth no-transmit sector",
            ControlId::NoTransmitStart1 => "Start angle of the (first) no-transmit sector",
            ControlId::NoTransmitStart2 => "Start angle of the second no-transmit sector",
            ControlId::NoTransmitStart3 => "Start angle of the third no-transmit sector",
            ControlId::NoTransmitStart4 => "Start angle of the fourth no-transmit sector",
            ControlId::Power => "Radar operational state",
            ControlId::WarmupTime => {
                "How long the radar still needs to warm up before transmitting"
            }
            ControlId::Range => "Maximum distance the radar is looking at",
            ControlId::Sea => "Sea clutter suppression",
            ControlId::SeaState => "Sea state for sea clutter suppression",
            ControlId::Rain => "Rain clutter suppression",
            ControlId::TargetTrails => "Whether target trails are shown",
            ControlId::TrailsMotion => "How target trails behave",
            ControlId::NoiseRejection => "Filters out noise",
            ControlId::TargetBoost => "Level of how much small targets are boosted",
            ControlId::TargetExpansion => "Increases target length for small targets",
            ControlId::TargetSeparation => "Makes separation between targets more prominent",
            ControlId::ScanSpeed => "Desired rotation speed of the radar antenna",
            ControlId::SideLobeSuppression => "Level of side lobe suppression",
            ControlId::Tune => "Method to finely tune the radar receiver",
            ControlId::SeaClutterCurve => "Sea clutter curve",
            ControlId::RotationSpeed => "How quickly the radar antenna rotates",
            ControlId::SignalStrength => "Signal strength of the radar",
            ControlId::OperatingTime => "How long the radar has been operating over its lifetime",
            ControlId::TransmitTime => "How long the radar has been transmitting over its lifetime",
            ControlId::SerialNumber => "Manufacturer serial number of the radar",
            ControlId::Spokes => "How many spokes the radar transmitted last rotation",
            ControlId::UserName => "User defined name for the radar",
        }
    }

    fn get_name(&self) -> &'static str {
        match self {
            ControlId::AccentLight => "Accent light",
            // ControlId::AllAuto => "All to Auto",
            // ControlId::AntennaForward => "Antenna forward of GPS",
            ControlId::AntennaHeight => "Antenna height",
            // ControlId::AntennaStarboard => "Antenna starboard of GPS",
            ControlId::BearingAlignment => "Bearing alignment",
            // ControlId::ColorGain => "Color gain",
            ControlId::ClearTargets => "Clear targets",
            ControlId::ClearTrails => "Clear trails",
            ControlId::ColorGain => "Color gain",
            ControlId::DisplayTiming => "Display timing",
            ControlId::Doppler => "Doppler",
            ControlId::DopplerMode => "Doppler mode",
            ControlId::DopplerAutoTrack => "Doppler Auto Track",
            ControlId::DopplerSpeedThreshold => "Doppler speed threshold",
            ControlId::DopplerTrailsOnly => "Doppler trails only",
            ControlId::FirmwareVersion => "Firmware version",
            ControlId::Ftc => "FTC",
            ControlId::Gain => "Gain",
            ControlId::InterferenceRejection => "Interference rejection",
            ControlId::LocalInterferenceRejection => "Local interference rejection",
            ControlId::BirdMode => "Bird mode",
            // ControlId::MainBangSize => "Main bang size",
            ControlId::MagnetronCurrent => "Magnetron current",
            ControlId::MainBangSuppression => "Main bang suppression",
            ControlId::Mode => "Mode",
            ControlId::ModelName => "Model name",
            ControlId::NoTransmitEnd1 => "No Transmit end",
            ControlId::NoTransmitEnd2 => "No Transmit end (2)",
            ControlId::NoTransmitEnd3 => "No Transmit end (3)",
            ControlId::NoTransmitEnd4 => "No Transmit end (4)",
            ControlId::NoTransmitStart1 => "No Transmit start",
            ControlId::NoTransmitStart2 => "No Transmit start (2)",
            ControlId::NoTransmitStart3 => "No Transmit start (3)",
            ControlId::NoTransmitStart4 => "No Transmit start (4)",
            ControlId::NoiseRejection => "Noise rejection",
            ControlId::OperatingTime => "Operating time",
            ControlId::TransmitTime => "Transmit time",
            // ControlId::Orientation => "Orientation",
            ControlId::Rain => "Rain clutter",
            ControlId::Range => "Range",
            ControlId::RotationSpeed => "Rotation speed",
            // ControlId::Scaling => "Scaling",
            ControlId::ScanSpeed => "Scan speed",
            ControlId::Sea => "Sea clutter",
            ControlId::SeaClutterCurve => "Sea clutter curve",
            ControlId::SeaState => "Sea state",
            ControlId::SerialNumber => "Serial Number",
            ControlId::SideLobeSuppression => "Side lobe suppression",
            // ControlId::Stc => "Sensitivity Time Control",
            // ControlId::StcCurve => "STC curve",
            ControlId::SignalStrength => "Signal strength",
            ControlId::Power => "Power",
            ControlId::TargetBoost => "Target boost",
            ControlId::TargetExpansion => "Target expansion",
            ControlId::TargetSeparation => "Target separation",
            ControlId::TargetTrails => "Target trails",
            // ControlId::TimedIdle => "Time idle",
            // ControlId::TimedRun => "Timed run",
            ControlId::TrailsMotion => "Target trails motion",
            ControlId::Tune => "Tune",
            // ControlId::TuneFine => "Fine tune",
            ControlId::Spokes => "Spokes",
            ControlId::UserName => "Custom name",
            ControlId::WarmupTime => "Warmup time",
        }
    }

    pub(crate) fn get_destination(&self) -> ControlDestination {
        match self {
            ControlId::AccentLight => ControlDestination::Command,
            ControlId::AntennaHeight => ControlDestination::Command,
            ControlId::BearingAlignment => ControlDestination::Command,
            ControlId::BirdMode => ControlDestination::Command,
            ControlId::ClearTargets => ControlDestination::Target,
            ControlId::ClearTrails => ControlDestination::Trail,
            ControlId::ColorGain => ControlDestination::Command,
            ControlId::DisplayTiming => ControlDestination::Command,
            ControlId::Power => ControlDestination::Command,
            ControlId::WarmupTime => ControlDestination::ReadOnly,
            ControlId::Range => ControlDestination::Command,
            ControlId::Mode => ControlDestination::Command,
            ControlId::Gain => ControlDestination::Command,
            ControlId::Sea => ControlDestination::Command,
            ControlId::SeaState => ControlDestination::Command,
            ControlId::Rain => ControlDestination::Command,
            ControlId::Doppler => ControlDestination::Command,
            ControlId::DopplerMode => ControlDestination::Command,
            ControlId::DopplerAutoTrack => ControlDestination::Target,
            ControlId::DopplerSpeedThreshold => ControlDestination::Command,
            ControlId::TargetTrails => ControlDestination::Trail,
            ControlId::TrailsMotion => ControlDestination::Trail,
            ControlId::DopplerTrailsOnly => ControlDestination::Trail,
            ControlId::NoiseRejection => ControlDestination::Command,
            ControlId::TargetBoost => ControlDestination::Command,
            ControlId::TargetExpansion => ControlDestination::Command,
            ControlId::InterferenceRejection => ControlDestination::Command,
            ControlId::TargetSeparation => ControlDestination::Command,
            ControlId::LocalInterferenceRejection => ControlDestination::Command,
            ControlId::ScanSpeed => ControlDestination::Command,
            ControlId::SideLobeSuppression => ControlDestination::Command,
            ControlId::Tune => ControlDestination::Command,
            ControlId::Ftc => ControlDestination::Command,
            ControlId::MainBangSuppression => ControlDestination::Command,
            ControlId::SeaClutterCurve => ControlDestination::Command,
            ControlId::NoTransmitStart1 => ControlDestination::Command,
            ControlId::NoTransmitEnd1 => ControlDestination::Command,
            ControlId::NoTransmitStart2 => ControlDestination::Command,
            ControlId::NoTransmitEnd2 => ControlDestination::Command,
            ControlId::NoTransmitStart3 => ControlDestination::Command,
            ControlId::NoTransmitEnd3 => ControlDestination::Command,
            ControlId::NoTransmitStart4 => ControlDestination::Command,
            ControlId::NoTransmitEnd4 => ControlDestination::Command,
            ControlId::RotationSpeed => ControlDestination::Command,
            ControlId::MagnetronCurrent => ControlDestination::Command,
            ControlId::SignalStrength => ControlDestination::Command,
            ControlId::OperatingTime => ControlDestination::ReadOnly,
            ControlId::TransmitTime => ControlDestination::ReadOnly,
            ControlId::ModelName => ControlDestination::ReadOnly,
            ControlId::FirmwareVersion => ControlDestination::ReadOnly,
            ControlId::SerialNumber => ControlDestination::ReadOnly,
            ControlId::Spokes => ControlDestination::ReadOnly,
            ControlId::UserName => ControlDestination::Internal,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub enum Category {
    Base,
    Advanced,
    Installation,
    Info,
}

#[derive(Error, Debug)]
pub enum ControlError {
    #[error("Control {0} not supported on this radar")]
    NotSupported(ControlId),
    #[error("Control {0} value {1} is lower than minimum value {2}")]
    TooLow(ControlId, f64, f64),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(ControlId, f64, f64),
    #[error("Control {0} value {1} is not a legal value")]
    Invalid(ControlId, String),
    #[error("Control {0} does not support Auto")]
    NoAuto(ControlId),
    #[error("Control {0} value '{1}' requires true heading input")]
    NoHeading(ControlId, &'static str),
    #[error("Control {0} value '{1}' requires a GNSS position")]
    NoPosition(ControlId, &'static str),
}

impl<'de> Deserialize<'de> for ControlId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ControlIdVisitor)
    }
}

struct ControlIdVisitor;

impl<'de> Visitor<'de> for ControlIdVisitor {
    type Value = ControlId;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string containing a ControlId name or numeric discriminant")
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Borrowed(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Borrowed(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_id(Cow::Owned(v))
    }
}

fn parse_control_id<E>(s: Cow<'_, str>) -> Result<ControlId, E>
where
    E: serde::de::Error,
{
    // Numeric discriminant encoded as string
    if let Ok(num) = s.parse::<u8>() {
        return match FromPrimitive::from_u8(num) {
            Some(ct) => Ok(ct),
            None => Err(E::custom("invalid ControlId discriminant")),
        };
    }

    // Case-insensitive name lookup (Strum)
    ControlId::from_str(&s).map_err(|_| E::custom("invalid ControlId name"))
}

impl Serialize for ControlId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match get_api_version() {
            ApiVersion::V3 => {
                // camelCase string, zero allocation
                let name: &'static str = (*self).into();
                log::debug!("Serializing V3 ControlId {:?} as string {}", self, name);

                serializer.serialize_str(name)
            }
            ApiVersion::V1 => {
                // numeric discriminant rendered as string
                // stack-only formatting

                let mut buf = itoa::Buffer::new();
                let s = buf.format(*self as u8);

                log::debug!("Serializing V1 ControlId {:?} as number {}", self, s);
                serializer.serialize_str(s)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use clap::Parser;

    use super::*;

    #[test]
    fn serialize_control_value() {
        // Check that the ControlValue serializes correctly
        let ct = ControlId::Gain;
        let cts = ct.to_string();
        assert_eq!(cts, "gain".to_string());
        println!("ControlId as string: {}", cts);
        match ControlId::parse_str(Cow::Owned(cts)) {
            Ok(c) => {
                assert_eq!(c, ct);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check with optional fields and V3 ID
        let json = r#"{"id":"gain","value":"49","auto":true,"enabled":false}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlId::Gain);
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, Some(true));
                assert_eq!(cv.enabled, Some(false));
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check without optional fields and with v1 ID
        let json = r#"{"id":"4","value":"49"}"#;

        match serde_json::from_str::<ControlValue>(&json) {
            Ok(cv) => {
                assert_eq!(cv.id, ControlId::Gain);
                assert_eq!(cv.value, "49");
                assert_eq!(cv.auto, None);
                assert_eq!(cv.enabled, None);
            }
            Err(e) => {
                panic!("Error {e}");
            }
        }

        // Check with illegal negative v1 ID
        let json = r#"{"id":"-1","value":"49"}"#;

        assert!(serde_json::from_str::<ControlValue>(&json).is_err());
    }

    #[test]
    fn control_range_values() {
        let args = Cli::parse_from(["my_program"]);
        let controls = SharedControls::new(&args, HashMap::new());

        assert!(controls.set(&ControlId::TargetTrails, 0., None).is_ok());
        assert_eq!(
            controls.set(&ControlId::TargetTrails, 6., None).unwrap(),
            Some(())
        );
        assert!(controls.set(&ControlId::TargetTrails, 7., None).is_err());
        assert!(controls.set(&ControlId::TargetTrails, -1., None).is_err());
        assert!(controls.set(&ControlId::TargetTrails, 0.3, None).is_ok());

        assert!(
            controls
                .set_value(&ControlId::TargetTrails, Value::String("3".to_string()))
                .is_ok()
        );
        assert_eq!(
            controls.get(&ControlId::TargetTrails).unwrap().value,
            Some(3.)
        );
    }
}
