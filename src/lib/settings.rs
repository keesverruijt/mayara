use num_derive::{FromPrimitive, ToPrimitive};
use num_traits::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Deserializer, Serialize, de::Visitor, ser::Serializer};
use serde_json::{Number, Value};
use std::cell::RefCell;
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
    controls: HashMap<ControlType, Control>,

    #[serde(skip)]
    radar_id: Option<String>,
    #[serde(skip)]
    all_clients_tx: tokio::sync::broadcast::Sender<ControlValue>,
    #[serde(skip)]
    sk_client_tx: Option<tokio::sync::broadcast::Sender<RadarControlValue>>,
    #[serde(skip)]
    control_update_tx: tokio::sync::broadcast::Sender<ControlUpdate>,
}

impl Controls {
    pub(self) fn insert(&mut self, control_type: ControlType, value: Control) {
        let v = Control {
            item: ControlDefinition {
                is_read_only: self.replay || value.item.is_read_only,
                ..value.item
            },
            ..value
        };

        self.controls.insert(control_type, v);
    }

    pub(self) fn new_base(args: &Cli, mut controls: HashMap<ControlType, Control>) -> Self {
        // Add _mandatory_ controls
        if !controls.contains_key(&ControlType::ModelName) {
            controls.insert(
                ControlType::ModelName,
                Control::new_string(ControlType::ModelName).read_only(true),
            );
        }

        if args.replay {
            controls.iter_mut().for_each(|(_k, v)| {
                v.item.is_read_only = true;
            });
        }

        // Add controls that are not radar dependent

        controls.insert(
            ControlType::UserName,
            Control::new_string(ControlType::UserName).read_only(false),
        );

        if args.targets != TargetMode::None {
            controls.insert(
                ControlType::TargetTrails,
                Control::new_map(
                    ControlType::TargetTrails,
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
                ControlType::TrailsMotion,
                Control::new_map(
                    ControlType::TrailsMotion,
                    HashMap::from([(0, "Relative".to_string()), (1, "True".to_string())]),
                ),
            );

            controls.insert(
                ControlType::ClearTrails,
                Control::new_button(ControlType::ClearTrails),
            );

            if args.targets == TargetMode::Arpa {
                controls.insert(
                    ControlType::ClearTargets,
                    Control::new_button(ControlType::ClearTargets),
                );
            }
        }

        let (all_clients_tx, _) = tokio::sync::broadcast::channel(32);
        let (control_update_tx, _) = tokio::sync::broadcast::channel(32);

        Controls {
            replay: args.replay,
            controls,
            radar_id: None,
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
    pub fn new(args: &Cli, mut controls: HashMap<ControlType, Control>) -> SharedControls {
        // All radars must have the same Status control
        let mut control = Control::new_list(
            ControlType::Power,
            &["Off", "Standby", "Transmit", "Preparing"],
        )
        .send_always();
        control.set_valid_values([1, 2].to_vec()); // Only allow setting to Standby (index 1) and Transmit (index 2)
        controls.insert(ControlType::Power, control);

        SharedControls {
            controls: Arc::new(RwLock::new(Controls::new_base(args, controls))),
        }
    }

    pub(crate) fn set_radar_info(
        &mut self,
        sk_client_tx: tokio::sync::broadcast::Sender<RadarControlValue>,
        radar_id: String,
    ) {
        let mut locked = self.controls.write().unwrap();
        locked.radar_id = Some(radar_id);
        locked.sk_client_tx = Some(sk_client_tx);
    }

    fn get_command_tx(&self) -> tokio::sync::broadcast::Sender<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.clone()
    }

    pub fn get_controls(&self) -> Option<HashMap<ControlType, Control>> {
        match self.controls.read() {
            Ok(locked) => Some(locked.controls.clone()),
            Err(_) => None,
        }
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
    pub async fn process_client_request(
        &self,
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
        let control = self.get(&control_value.id);

        if let Err(e) = match control {
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
                        self.send_to_command_handler(control_value.clone(), reply_tx.clone())
                    }
                    ControlDestination::ReadOnly => {
                        Err(RadarError::CannotSetControlType(control_value.id))
                    }
                }
            }
            None => Err(RadarError::CannotSetControlType(control_value.id)),
        } {
            self.send_error_to_client(reply_tx, &control_value, &e)
                .await
        } else {
            Ok(())
        }
    }

    pub fn control_update_subscribe(&self) -> tokio::sync::broadcast::Receiver<ControlUpdate> {
        let locked = self.controls.read().unwrap();

        locked.control_update_tx.subscribe()
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
        control_value: ControlValue,
        reply_tx: tokio::sync::mpsc::Sender<ControlValue>,
    ) -> Result<(), RadarError> {
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
        let control_value = crate::settings::ControlValue {
            id: control.item().control_type,
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            dynamic_read_only: control.dynamic_read_only,
            error: None,
        };
        let locked = self.controls.read().unwrap();
        match locked.all_clients_tx.send(control_value.clone()) {
            Err(_e) => {}
            Ok(cnt) => {
                log::trace!(
                    "Sent control value {} to {} JSON clients",
                    control.item().control_type,
                    cnt
                );
            }
        }
        if let (Some(radar_id), Some(sk_client_tx)) = (&locked.radar_id, &locked.sk_client_tx) {
            let radar_control_value = RadarControlValue::new(radar_id.to_string(), control_value);

            match sk_client_tx.send(radar_control_value) {
                Err(_e) => {}
                Ok(cnt) => {
                    log::trace!(
                        "Sent control value {} to {} SK clients",
                        control.item().control_type,
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
            Err(RadarError::CannotSetControlType(cv.id))
        }
    }

    // ******* GET & SET METHODS

    pub fn insert(&self, control_type: ControlType, value: Control) {
        let mut locked = self.controls.write().unwrap();

        locked.insert(control_type, value);
    }

    pub fn get(&self, control_type: &ControlType) -> Option<Control> {
        let locked = self.controls.read().unwrap();

        locked.controls.get(control_type).cloned()
    }

    pub fn get_by_id(&self, control_id: &str) -> Option<Control> {
        match ControlType::parse_str(Cow::Borrowed(control_id)) {
            Ok(cv) => self.get(&cv),
            Err(_) => None,
        }
    }

    pub fn get_control_keys(&self) -> Vec<&'static str> {
        let locked = self.controls.read().unwrap();

        locked.controls.iter().map(|(k, _)| k.into()).collect()
    }

    pub fn contains_key(&self, control_type: &ControlType) -> bool {
        let locked = self.controls.read().unwrap();

        locked.controls.contains_key(control_type)
    }

    pub fn set_refresh(&self, control_type: &ControlType) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_type) {
            control.needs_refresh = true;
        }
    }

    pub fn set_dynamic_read_only(&self, control_type: &ControlType, read_only: bool) {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_type) {
            control.set_dynamic_read_only(read_only);
        }
    }

    pub fn set_value_auto_enabled<T>(
        &self,
        control_type: &ControlType,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) -> Result<Option<()>, ControlError>
    where
        f32: From<T>,
    {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
                Ok(control
                    .set(value.into(), None, auto, enabled)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
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
        control_type: &ControlType,
        min: f32,
        max: f32,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
                Ok(control.set_wire_range(min, max)?.map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
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
        control_type: &ControlType,
        value: f32,
        auto: Option<bool>,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
                Ok(control
                    .set(value, None, auto, None)?
                    .map(|_| control.clone()))
            } else {
                Err(ControlError::NotSupported(*control_type))
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

    pub fn set_auto_state(
        &self,
        control_type: &ControlType,
        auto: bool,
    ) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        if let Some(control) = locked.controls.get_mut(control_type) {
            control.set_auto(auto);
        } else {
            return Err(ControlError::NotSupported(*control_type));
        };
        Ok(())
    }

    pub fn set_value_auto(
        &self,
        control_type: &ControlType,
        auto: bool,
        value: f32,
    ) -> Result<Option<()>, ControlError> {
        self.set(control_type, value, Some(auto))
    }

    pub fn set_value_with_many_auto(
        &self,
        control_type: &ControlType,
        value: f32,
        auto_value: f32,
    ) -> Result<Option<()>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
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
            self.send_to_all_clients(&control);
            Ok(Some(()))
        } else {
            Ok(None)
        }
    }

    pub fn set_string(
        &self,
        control_type: &ControlType,
        value: String,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
                if control.item().data_type == ControlDataType::String {
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
            self.send_to_all_clients(&control);
            Ok(control.description.clone())
        } else {
            Ok(None)
        }
    }

    pub fn set_value(
        &self,
        control_type: &ControlType,
        value: Value,
    ) -> Result<Option<String>, ControlError> {
        let control = {
            let mut locked = self.controls.write().unwrap();
            if let Some(control) = locked.controls.get_mut(control_type) {
                if control.item().data_type == ControlDataType::String {
                    match value {
                        Value::String(s) => Ok(control.set_string(s).map(|_| control.clone())),
                        _ => Err(ControlError::Invalid(*control_type, format!("{:?}", value))),
                    }
                } else {
                    match value.clone() {
                        Value::String(s) => {
                            let i = s.parse::<i32>().map_err(|_| {
                                ControlError::Invalid(control_type.clone(), format!("{:?}", value))
                            })?;
                            control
                                .set(i as f32, None, None, None)
                                .map(|_| Some(control.clone()))
                        }
                        Value::Bool(b) => {
                            let i = b as i32 as f32;
                            control
                                .set(i as f32, None, None, None)
                                .map(|_| Some(control.clone()))
                        }
                        Value::Number(n) => match n.as_f64() {
                            Some(n) => control
                                .set(n as f32, None, None, None)
                                .map(|_| Some(control.clone())),
                            None => Err(ControlError::Invalid(
                                control_type.clone(),
                                format!("{:?}", value),
                            )),
                        },
                        _ => Err(ControlError::Invalid(
                            control_type.clone(),
                            format!("{:?}", value),
                        )),
                    }
                }
            } else {
                Err(ControlError::NotSupported(*control_type))
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
        let control = locked.controls.get_mut(&ControlType::UserName).unwrap();
        control.set_string(name);
    }

    pub fn user_name(&self) -> String {
        self.get(&ControlType::UserName)
            .and_then(|c| c.description)
            .unwrap()
    }

    pub fn set_model_name(&self, name: String) {
        let mut locked = self.controls.write().unwrap();
        let control = locked.controls.get_mut(&ControlType::ModelName).unwrap();
        control.set_string(name.clone());
    }

    pub fn model_name(&self) -> Option<String> {
        self.get(&ControlType::ModelName)
            .and_then(|c| c.description)
    }

    pub fn set_valid_values(
        &self,
        control_type: &ControlType,
        valid_values: Vec<i32>,
    ) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(control_type)
            .ok_or(ControlError::NotSupported(*control_type))
            .map(|c| {
                c.set_valid_values(valid_values);
                ()
            })
    }

    pub fn set_valid_ranges(
        &self,
        control_type: &ControlType,
        ranges: &Ranges,
    ) -> Result<(), ControlError> {
        let mut locked = self.controls.write().unwrap();
        locked
            .controls
            .get_mut(control_type)
            .ok_or(ControlError::NotSupported(*control_type))
            .map(|c| {
                c.set_valid_ranges(ranges);
                ()
            })?;
        Ok(())
    }

    pub(crate) fn get_status(&self) -> Option<Power> {
        let locked = self.controls.read().unwrap();
        if let Some(control) = locked.controls.get(&ControlType::Power) {
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

// This is what we send back and forth internally between (web) clients and radar managers for v1
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ControlValue {
    pub id: ControlType,
    pub value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub dynamic_read_only: Option<bool>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlValue {
    pub fn new(id: ControlType, value: Value) -> Self {
        ControlValue {
            id,
            value,
            auto: None,
            enabled: None,
            dynamic_read_only: None,
            error: None,
        }
    }

    pub fn from(control: &Control, error: Option<String>) -> Self {
        ControlValue {
            id: control.item().control_type,
            value: control.value(),
            auto: control.auto,
            enabled: control.enabled,
            dynamic_read_only: None,
            error,
        }
    }

    pub fn as_bool(&self) -> Result<bool, RadarError> {
        self.as_i64().map(|n| n != 0)
    }

    pub fn as_i32(&self) -> Result<i32, RadarError> {
        self.as_i64()?
            .try_into()
            .map_err(|_| RadarError::CannotSetControlTypeValue(self.id, self.value.clone()))
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
        .map_err(|_| RadarError::CannotSetControlTypeValue(self.id, self.value.clone()))
    }

    pub fn as_f32(&self) -> Result<f32, RadarError> {
        self.as_f64().map(|n| n as f32)
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
        .map_err(|_| RadarError::CannotSetControlTypeValue(self.id, self.value.clone()))
    }
}

// This is the represenation of a control value used by the Signal K (web) services
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RadarControlValue {
    pub radar_id: String,
    #[serde(flatten)]
    pub control_value: ControlValue,
}

impl RadarControlValue {
    pub fn new(radar_id: String, control_value: ControlValue) -> Self {
        RadarControlValue {
            radar_id,
            control_value,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Control {
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
    pub dynamic_read_only: Option<bool>,
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
            dynamic_read_only: None,
            needs_refresh: false,
        }
    }

    pub(crate) fn read_only(mut self, is_read_only: bool) -> Self {
        self.item.is_read_only = is_read_only;

        self
    }

    pub fn set_dynamic_read_only(&mut self, dynamic_read_only: bool) {
        if self.dynamic_read_only != Some(dynamic_read_only) {
            self.dynamic_read_only = Some(dynamic_read_only);
            self.needs_refresh = true;
        }
    }

    pub(crate) fn wire_scale_step(mut self, step: f32) -> Self {
        self.item.step_value = Some(step);
        self.item.wire_scale_factor = Some(self.item.max_value.unwrap_or(1.) / step);

        self
    }

    pub(crate) fn wire_scale_factor(mut self, wire_scale_factor: f32, with_step: bool) -> Self {
        self.item.wire_scale_factor = Some(wire_scale_factor);
        if with_step {
            self.item.step_value =
                Some(self.item.max_value.unwrap_or(1.) / self.item.wire_scale_factor.unwrap_or(1.));
        }

        self
    }

    pub(crate) fn wire_offset(mut self, wire_offset: f32) -> Self {
        self.item.wire_offset = Some(wire_offset);

        self
    }

    pub(crate) fn unit<S: AsRef<str>>(mut self, unit: S) -> Control {
        self.item.unit = Some(unit.as_ref().to_string());

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

    pub(crate) fn new_numeric(control_type: ControlType, min_value: f32, max_value: f32) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        let control = Self::new(ControlDefinition::new(
            control_type,
            ControlDataType::Number,
            min_value,
            None,
            false,
            min_value,
            max_value,
            None,
            max_value,
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
        control_type: ControlType,
        min_value: f32,
        max_value: f32,
        automatic: AutomaticValue,
    ) -> Self {
        let min_value = Some(min_value);
        let max_value = Some(max_value);
        Self::new(ControlDefinition::new(
            control_type,
            ControlDataType::Number,
            min_value,
            Some(automatic),
            false,
            min_value,
            max_value,
            None,
            max_value,
            None,
            None,
            None,
            None,
            false,
            false,
        ))
    }

    pub(crate) fn new_list(control_type: ControlType, descriptions: &[&str]) -> Self {
        let description_count = ((descriptions.len() as i32) - 1) as f32;
        Self::new(ControlDefinition::new(
            control_type,
            ControlDataType::Number,
            Some(0.),
            None,
            false,
            Some(0.),
            Some(description_count),
            None,
            Some(description_count),
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

    pub(crate) fn new_map(control_type: ControlType, descriptions: HashMap<i32, String>) -> Self {
        Self::new(ControlDefinition::new(
            control_type,
            ControlDataType::Number,
            Some(0.),
            None,
            false,
            Some(0.),
            Some(((descriptions.len() as i32) - 1) as f32),
            None,
            Some(((descriptions.len() as i32) - 1) as f32),
            None,
            None,
            Some(descriptions),
            None,
            false,
            false,
        ))
    }

    pub(crate) fn new_string(control_type: ControlType) -> Self {
        Self::new(ControlDefinition::new(
            control_type,
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

    pub(crate) fn new_button(control_type: ControlType) -> Self {
        Self::new(ControlDefinition::new(
            control_type,
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

    fn to_number(&self, v: f32) -> Number {
        if let Some(n) = {
            if v == v as i32 as f32 {
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

        let v: f32 = self.value.unwrap_or(self.item.default_value.unwrap_or(0.));

        Value::Number(self.to_number(v))
    }

    pub fn as_f32(&self) -> Option<f32> {
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
            self.item.control_type,
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
        mut value: f32,
        mut auto_value: Option<f32>,
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
        if let (Some(wire_scale_factor), Some(max_value)) =
            (self.item.wire_scale_factor, self.item.max_value)
        {
            // One of the reasons we use f32 is because Navico wire format for some things is
            // tenths of degrees. To make things uniform we map these to a float with .1 precision.
            if wire_scale_factor != max_value {
                log::trace!("{} map value {}", self.item.control_type, value);
                value = value * max_value / wire_scale_factor;

                // TODO! Not sure about the following line
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

        let step = self.item.step_value.unwrap_or(1.0);
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

    /// Set the control's wire offset and scale
    ///
    /// Return Ok(Some(())) when the value changed or it always needs
    /// to be broadcast to listeners.
    ///
    pub(crate) fn set_wire_range(
        &mut self,
        min: f32,
        max: f32,
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
    pub(crate) auto_adjust_min_value: f32,
    pub(crate) auto_adjust_max_value: f32,
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
    pub control_type: ControlType,
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
    is_send_always: bool, // Whether the controlvalue is sent out to client in all state messages
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ControlDefinition {
    fn new(
        control_type: ControlType,
        data_type: ControlDataType,
        default_value: Option<f32>,
        automatic: Option<AutomaticValue>,
        has_enabled: bool,
        min_value: Option<f32>,
        max_value: Option<f32>,
        step_value: Option<f32>,
        wire_scale_factor: Option<f32>,
        wire_offset: Option<f32>,
        unit: Option<String>,
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

        ControlDefinition {
            id: control_type as u8,
            control_type,
            name: control_type.get_name(),
            description: control_type.get_description(),
            category: control_type.get_category(),
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
pub enum ControlType {
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
    OperatingHours,
    TransmitHours,
    ModelName,
    FirmwareVersion,
    SerialNumber,
    Spokes,
    UserName,
}

impl Display for ControlType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s: &'static str = self.into();

        write!(f, "{}", s)
    }
}

impl ControlType {
    pub fn from_u8(value: u8) -> ControlType {
        FromPrimitive::from_u8(value).unwrap()
    }

    pub fn parse_str(s: Cow<'_, str>) -> Result<ControlType, RadarError> {
        // Numeric discriminant encoded as string
        if let Ok(num) = s.parse::<u8>() {
            return match FromPrimitive::from_u8(num) {
                Some(ct) => Ok(ct),
                None => Err(RadarError::InvalidControlType(
                    "invalid ControlType discriminant".to_string(),
                )),
            };
        }

        // Case-insensitive name lookup (Strum)
        ControlType::from_str(&s).map_err(|_| {
            RadarError::InvalidControlType("invalid ControlType discriminant".to_string())
        })
    }

    pub fn get_category(&self) -> Category {
        match self {
            ControlType::Power
            | ControlType::BirdMode
            | ControlType::Range
            | ControlType::Mode
            | ControlType::Gain
            | ControlType::ColorGain
            | ControlType::Sea
            | ControlType::SeaState
            | ControlType::Rain
            | ControlType::Doppler
            | ControlType::DopplerMode
            | ControlType::TargetTrails
            | ControlType::ClearTargets
            | ControlType::ClearTrails => Category::Base,
            ControlType::AccentLight
            | ControlType::AntennaHeight
            | ControlType::BearingAlignment
            | ControlType::NoTransmitEnd1
            | ControlType::NoTransmitEnd2
            | ControlType::NoTransmitEnd3
            | ControlType::NoTransmitEnd4
            | ControlType::NoTransmitStart1
            | ControlType::NoTransmitStart2
            | ControlType::NoTransmitStart3
            | ControlType::NoTransmitStart4 => Category::Installation,
            ControlType::ModelName
            | ControlType::WarmupTime
            | ControlType::FirmwareVersion
            | ControlType::OperatingHours
            | ControlType::TransmitHours
            | ControlType::MagnetronCurrent
            | ControlType::RotationSpeed
            | ControlType::SerialNumber
            | ControlType::SignalStrength => Category::Info,
            _ => Category::Advanced,
        }
    }

    pub fn get_description(&self) -> &'static str {
        match self {
            ControlType::AccentLight => "Strength of the accent light",
            ControlType::AntennaHeight => "Height of the antenna above waterline",
            ControlType::BearingAlignment => {
                "Alignment of the antenna relative to the vessel's bow"
            }
            ControlType::ClearTargets => "Clear all ARPA targets",
            ControlType::ClearTrails => "Clear target trails",
            ControlType::ColorGain => "Adjust the color curve relative to gain",
            ControlType::DisplayTiming => "Display timing",
            ControlType::Doppler => {
                "Targets coming towards or going away from own ship shown in different colors"
            }
            ControlType::DopplerMode => "For what type of targets Doppler is used",
            ControlType::DopplerAutoTrack => {
                "Convert all Doppler targets to ARPA targets automatically"
            }
            ControlType::DopplerSpeedThreshold => "Threshold speed above which Doppler is applied",
            ControlType::DopplerTrailsOnly => "Convert only Doppler targets to target trails",
            ControlType::FirmwareVersion => "Version of the radar firmware",
            ControlType::Ftc => "FTC",
            ControlType::Gain => "How sensitive the radar is to returning echoes",
            ControlType::InterferenceRejection => "Reduces interference from other radars",
            ControlType::LocalInterferenceRejection => {
                "How much local interference rejection is applied"
            }
            ControlType::BirdMode => "Level of optimization for bird targets",
            ControlType::MagnetronCurrent => "The current supplied to the magnetron",
            ControlType::MainBangSuppression => "Main bang suppression",
            ControlType::Mode => "Choice of radar mode tuning to certain conditions, or custom",
            ControlType::ModelName => "Manufacturer model name of the radar",
            ControlType::NoTransmitEnd1 => "End angle of the (first) no-transmit sector",
            ControlType::NoTransmitEnd2 => "End angle of the second no-transmit sector",
            ControlType::NoTransmitEnd3 => "End angle of the third no-transmit sector",
            ControlType::NoTransmitEnd4 => "End angle of the fourth no-transmit sector",
            ControlType::NoTransmitStart1 => "Start angle of the (first) no-transmit sector",
            ControlType::NoTransmitStart2 => "Start angle of the second no-transmit sector",
            ControlType::NoTransmitStart3 => "Start angle of the third no-transmit sector",
            ControlType::NoTransmitStart4 => "Start angle of the fourth no-transmit sector",
            ControlType::Power => "Radar operational state",
            ControlType::WarmupTime => {
                "How long the radar still needs to warm up before transmitting"
            }
            ControlType::Range => "Maximum distance the radar is looking at",
            ControlType::Sea => "Sea clutter suppression",
            ControlType::SeaState => "Sea state for sea clutter suppression",
            ControlType::Rain => "Rain clutter suppression",
            ControlType::TargetTrails => "Whether target trails are shown",
            ControlType::TrailsMotion => "How target trails behave",
            ControlType::NoiseRejection => "Filters out noise",
            ControlType::TargetBoost => "Level of how much small targets are boosted",
            ControlType::TargetExpansion => "Increases target length for small targets",
            ControlType::TargetSeparation => "Makes separation between targets more prominent",
            ControlType::ScanSpeed => "Desired rotation speed of the radar antenna",
            ControlType::SideLobeSuppression => "Level of side lobe suppression",
            ControlType::Tune => "Method to finely tune the radar receiver",
            ControlType::SeaClutterCurve => "Sea clutter curve",
            ControlType::RotationSpeed => "How quickly the radar antenna rotates",
            ControlType::SignalStrength => "Signal strength of the radar",
            ControlType::OperatingHours => "How many hours the radar has been operating",
            ControlType::TransmitHours => "How many hours the radar has been transmitting",
            ControlType::SerialNumber => "Manufacturer serial number of the radar",
            ControlType::Spokes => "How many spokes the radar transmits per rotation",
            ControlType::UserName => "User defined name for the radar",
        }
    }

    fn get_name(&self) -> &'static str {
        match self {
            ControlType::AccentLight => "Accent light",
            // ControlType::AllAuto => "All to Auto",
            // ControlType::AntennaForward => "Antenna forward of GPS",
            ControlType::AntennaHeight => "Antenna height",
            // ControlType::AntennaStarboard => "Antenna starboard of GPS",
            ControlType::BearingAlignment => "Bearing alignment",
            // ControlType::ColorGain => "Color gain",
            ControlType::ClearTargets => "Clear targets",
            ControlType::ClearTrails => "Clear trails",
            ControlType::ColorGain => "Color gain",
            ControlType::DisplayTiming => "Display timing",
            ControlType::Doppler => "Doppler",
            ControlType::DopplerMode => "Doppler mode",
            ControlType::DopplerAutoTrack => "Doppler Auto Track",
            ControlType::DopplerSpeedThreshold => "Doppler speed threshold",
            ControlType::DopplerTrailsOnly => "Doppler trails only",
            ControlType::FirmwareVersion => "Firmware version",
            ControlType::Ftc => "FTC",
            ControlType::Gain => "Gain",
            ControlType::InterferenceRejection => "Interference rejection",
            ControlType::LocalInterferenceRejection => "Local interference rejection",
            ControlType::BirdMode => "Bird mode",
            // ControlType::MainBangSize => "Main bang size",
            ControlType::MagnetronCurrent => "Magnetron current",
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
            ControlType::TransmitHours => "Transmit hours",
            // ControlType::Orientation => "Orientation",
            ControlType::Rain => "Rain clutter",
            ControlType::Range => "Range",
            ControlType::RotationSpeed => "Rotation speed",
            // ControlType::Scaling => "Scaling",
            ControlType::ScanSpeed => "Scan speed",
            ControlType::Sea => "Sea clutter",
            ControlType::SeaClutterCurve => "Sea clutter curve",
            ControlType::SeaState => "Sea state",
            ControlType::SerialNumber => "Serial Number",
            ControlType::SideLobeSuppression => "Side lobe suppression",
            // ControlType::Stc => "Sensitivity Time Control",
            // ControlType::StcCurve => "STC curve",
            ControlType::SignalStrength => "Signal strength",
            ControlType::Power => "Power",
            ControlType::TargetBoost => "Target boost",
            ControlType::TargetExpansion => "Target expansion",
            ControlType::TargetSeparation => "Target separation",
            ControlType::TargetTrails => "Target trails",
            // ControlType::TimedIdle => "Time idle",
            // ControlType::TimedRun => "Timed run",
            ControlType::TrailsMotion => "Target trails motion",
            ControlType::Tune => "Tune",
            // ControlType::TuneFine => "Fine tune",
            ControlType::Spokes => "Spokes",
            ControlType::UserName => "Custom name",
            ControlType::WarmupTime => "Warmup time",
        }
    }

    pub(crate) fn get_destination(&self) -> ControlDestination {
        match self {
            ControlType::AccentLight => ControlDestination::Command,
            ControlType::AntennaHeight => ControlDestination::Command,
            ControlType::BearingAlignment => ControlDestination::Command,
            ControlType::BirdMode => ControlDestination::Command,
            ControlType::ClearTargets => ControlDestination::Target,
            ControlType::ClearTrails => ControlDestination::Trail,
            ControlType::ColorGain => ControlDestination::Command,
            ControlType::DisplayTiming => ControlDestination::Command,
            ControlType::Power => ControlDestination::Command,
            ControlType::WarmupTime => ControlDestination::ReadOnly,
            ControlType::Range => ControlDestination::Command,
            ControlType::Mode => ControlDestination::Command,
            ControlType::Gain => ControlDestination::Command,
            ControlType::Sea => ControlDestination::Command,
            ControlType::SeaState => ControlDestination::Command,
            ControlType::Rain => ControlDestination::Command,
            ControlType::Doppler => ControlDestination::Command,
            ControlType::DopplerMode => ControlDestination::Command,
            ControlType::DopplerAutoTrack => ControlDestination::Target,
            ControlType::DopplerSpeedThreshold => ControlDestination::Command,
            ControlType::TargetTrails => ControlDestination::Trail,
            ControlType::TrailsMotion => ControlDestination::Trail,
            ControlType::DopplerTrailsOnly => ControlDestination::Trail,
            ControlType::NoiseRejection => ControlDestination::Command,
            ControlType::TargetBoost => ControlDestination::Command,
            ControlType::TargetExpansion => ControlDestination::Command,
            ControlType::InterferenceRejection => ControlDestination::Command,
            ControlType::TargetSeparation => ControlDestination::Command,
            ControlType::LocalInterferenceRejection => ControlDestination::Command,
            ControlType::ScanSpeed => ControlDestination::Command,
            ControlType::SideLobeSuppression => ControlDestination::Command,
            ControlType::Tune => ControlDestination::Command,
            ControlType::Ftc => ControlDestination::Command,
            ControlType::MainBangSuppression => ControlDestination::Command,
            ControlType::SeaClutterCurve => ControlDestination::Command,
            ControlType::NoTransmitStart1 => ControlDestination::Command,
            ControlType::NoTransmitEnd1 => ControlDestination::Command,
            ControlType::NoTransmitStart2 => ControlDestination::Command,
            ControlType::NoTransmitEnd2 => ControlDestination::Command,
            ControlType::NoTransmitStart3 => ControlDestination::Command,
            ControlType::NoTransmitEnd3 => ControlDestination::Command,
            ControlType::NoTransmitStart4 => ControlDestination::Command,
            ControlType::NoTransmitEnd4 => ControlDestination::Command,
            ControlType::RotationSpeed => ControlDestination::Command,
            ControlType::MagnetronCurrent => ControlDestination::Command,
            ControlType::SignalStrength => ControlDestination::Command,
            ControlType::OperatingHours => ControlDestination::ReadOnly,
            ControlType::TransmitHours => ControlDestination::ReadOnly,
            ControlType::ModelName => ControlDestination::ReadOnly,
            ControlType::FirmwareVersion => ControlDestination::ReadOnly,
            ControlType::SerialNumber => ControlDestination::ReadOnly,
            ControlType::Spokes => ControlDestination::ReadOnly,
            ControlType::UserName => ControlDestination::Internal,
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
    NotSupported(ControlType),
    #[error("Control {0} value {1} is lower than minimum value {2}")]
    TooLow(ControlType, f32, f32),
    #[error("Control {0} value {1} is higher than maximum value {2}")]
    TooHigh(ControlType, f32, f32),
    #[error("Control {0} value {1} is not a legal value")]
    Invalid(ControlType, String),
    #[error("Control {0} does not support Auto")]
    NoAuto(ControlType),
    #[error("Control {0} value '{1}' requires true heading input")]
    NoHeading(ControlType, &'static str),
    #[error("Control {0} value '{1}' requires a GNSS position")]
    NoPosition(ControlType, &'static str),
}

impl<'de> Deserialize<'de> for ControlType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(ControlTypeVisitor)
    }
}

struct ControlTypeVisitor;

impl<'de> Visitor<'de> for ControlTypeVisitor {
    type Value = ControlType;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string containing a ControlType name or numeric discriminant")
    }

    fn visit_borrowed_str<E>(self, v: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_type(Cow::Borrowed(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_type(Cow::Borrowed(v))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        parse_control_type(Cow::Owned(v))
    }
}

fn parse_control_type<E>(s: Cow<'_, str>) -> Result<ControlType, E>
where
    E: serde::de::Error,
{
    // Numeric discriminant encoded as string
    if let Ok(num) = s.parse::<u8>() {
        return match FromPrimitive::from_u8(num) {
            Some(ct) => Ok(ct),
            None => Err(E::custom("invalid ControlType discriminant")),
        };
    }

    // Case-insensitive name lookup (Strum)
    ControlType::from_str(&s).map_err(|_| E::custom("invalid ControlType name"))
}

impl Serialize for ControlType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match get_api_version() {
            ApiVersion::V3 => {
                // camelCase string, zero allocation
                let name: &'static str = (*self).into();
                log::debug!("Serializing V3 ControlType {:?} as string {}", self, name);

                serializer.serialize_str(name)
            }
            ApiVersion::V1 => {
                // numeric discriminant rendered as string
                // stack-only formatting

                let mut buf = itoa::Buffer::new();
                let s = buf.format(*self as u8);

                log::debug!("Serializing V1 ControlType {:?} as number {}", self, s);
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
        let ct = ControlType::Gain;
        let cts = ct.to_string();
        assert_eq!(cts, "gain".to_string());
        println!("ControlType as string: {}", cts);
        match ControlType::parse_str(Cow::Owned(cts)) {
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
                assert_eq!(cv.id, ControlType::Gain);
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
                assert_eq!(cv.id, ControlType::Gain);
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

        assert!(controls.set(&ControlType::TargetTrails, 0., None).is_ok());
        assert_eq!(
            controls.set(&ControlType::TargetTrails, 6., None).unwrap(),
            Some(())
        );
        assert!(controls.set(&ControlType::TargetTrails, 7., None).is_err());
        assert!(controls.set(&ControlType::TargetTrails, -1., None).is_err());
        assert!(controls.set(&ControlType::TargetTrails, 0.3, None).is_ok());

        assert!(
            controls
                .set_value(&ControlType::TargetTrails, Value::String("3".to_string()))
                .is_ok()
        );
        assert_eq!(
            controls.get(&ControlType::TargetTrails).unwrap().value,
            Some(3.)
        );
    }
}
