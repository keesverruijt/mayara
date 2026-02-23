use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    cmp::min,
    collections::HashMap,
    str::FromStr,
    time::{Duration, SystemTime},
};
use strum::{EnumString, IntoEnumIterator, VariantNames};
use utoipa::ToSchema;
use wildmatch::WildMatch;

use crate::{
    PACKAGE,
    radar::settings::{BareControlValue, Control, ControlDefinition, ControlId, RadarControlValue},
    radar::{RadarError, SharedRadars},
};

/// Server-to-client delta message containing control value updates
#[derive(Serialize, Clone, Debug, ToSchema)]
#[schema(example = json!({
    "updates": [{
        "$source": "mayara",
        "timestamp": "2024-01-15T10:30:00Z",
        "values": [
            {"path": "radars.nav1034A.controls.gain", "value": 50},
            {"path": "radars.nav1034A.controls.sea", "value": 30, "auto": true}
        ]
    }]
}))]
pub struct SignalKDelta {
    /// Array of update batches, each containing changed control values
    updates: Vec<DeltaUpdate>,
}

impl SignalKDelta {
    pub fn new() -> SignalKDelta {
        Self {
            updates: Vec::new(),
        }
    }

    //
    // Used when starting a websocket, we always check radars for unsent
    //
    pub fn add_meta_updates(&mut self, radars: &SharedRadars, meta_sent: &mut Vec<String>) {
        if let Some(updates) = get_meta_delta(radars, meta_sent) {
            self.updates.push(updates);
        }
    }

    //
    // Every time we send a SignalKDelta, we check for unsent meta data
    //
    pub fn add_meta_from_updates(&mut self, radars: &SharedRadars, meta_sent: &mut Vec<String>) {
        let mut meta = false;
        for update in &self.updates {
            for dv in &update.values {
                let radar_id = dv.path.split('.').nth(1).unwrap();
                if meta_sent.iter().any(|x| x == radar_id) {
                    meta = true;
                    break;
                }
            }
        }
        if !meta {
            self.add_meta_updates(radars, meta_sent);
        }
    }

    pub fn add_updates(&mut self, rcvs: Vec<RadarControlValue>) {
        let delta_update = DeltaUpdate::from(rcvs);
        self.updates.push(delta_update);
    }

    pub fn add_meta_for_control(&mut self, radar_id: &str, control: &Control) {
        let mut meta = Vec::new();
        let path = format!("radars.{}.controls.{}", radar_id, control.item().control_id);
        let value = control.item().clone();
        meta.push(DeltaMeta { path, value });

        let delta_update = DeltaUpdate {
            timestamp: Some(Utc::now()),
            source: Some(PACKAGE.to_string()),
            meta,
            values: Vec::new(),
        };
        self.updates.push(delta_update);
    }

    pub fn apply_subscriptions(&mut self, subscriptions: &mut ActiveSubscriptions) {
        for update in self.updates.iter_mut() {
            update
                .values
                .retain(|dv| subscriptions.is_subscribed_path(&dv.path, false));
        } // Modify the SK delta for the subscriptions active
    }

    pub fn build(self) -> Option<Self> {
        if self.updates.len() > 0 {
            return Some(self);
        }
        return None;
    }
}

/// A batch of control value updates within a SignalKDelta message
#[derive(Serialize, Clone, Debug, ToSchema)]
struct DeltaUpdate {
    /// Source identifier (always "mayara")
    #[serde(
        rename = "$source",
        skip_deserializing,
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(example = "mayara")]
    source: Option<String>,
    /// ISO 8601 timestamp when the update was generated
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = String, example = "2024-01-15T10:30:00Z")]
    timestamp: Option<DateTime<Utc>>,
    /// Control metadata (schema definitions, sent once per radar)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    meta: Vec<DeltaMeta>,
    /// Control value changes
    #[serde(skip_serializing_if = "Vec::is_empty")]
    values: Vec<DeltaValue>,
}

/// A single control value update
#[derive(Serialize, Clone, Debug, ToSchema)]
#[schema(example = json!({"path": "radars.nav1034A.controls.gain", "value": 50}))]
struct DeltaValue {
    /// Full path to the control (e.g., "radars.nav1034A.controls.gain")
    #[schema(example = "radars.nav1034A.controls.gain")]
    path: String,
    /// The control value
    value: BareControlValue,
}

impl DeltaUpdate {
    fn from(radar_control_values: Vec<RadarControlValue>) -> Self {
        let mut values = Vec::new();
        for radar_control_value in radar_control_values {
            let path = radar_control_value.path.to_string();

            let value = BareControlValue::from(radar_control_value);
            values.push(DeltaValue { path, value });
        }

        let delta_update = DeltaUpdate {
            timestamp: None,
            source: Some(PACKAGE.to_string()),
            meta: Vec::new(),
            values,
        };

        return delta_update;
    }
}

/// Control metadata containing schema definitions
#[derive(Serialize, Clone, Debug, ToSchema)]
pub struct DeltaMeta {
    /// Full path to the control
    #[schema(example = "radars.nav1034A.controls.gain")]
    path: String,
    /// Control definition including type, range, and valid values
    value: ControlDefinition,
}

fn get_meta_delta(radars: &SharedRadars, meta_sent: &mut Vec<String>) -> Option<DeltaUpdate> {
    let mut meta = Vec::new();

    for radar in radars.get_active() {
        let radar_id = radar.key();
        let controls = radar.controls.get_controls();

        for (k, v) in controls.iter() {
            let path = format!("radars.{}.controls.{}", radar_id, k);
            let value = v.item().clone();
            meta.push(DeltaMeta { path, value });
        }
        meta_sent.push(radar_id);
    }

    if meta.len() == 0 {
        return None;
    }
    let delta_update = DeltaUpdate {
        timestamp: Some(Utc::now()),
        source: Some(PACKAGE.to_string()),
        meta,
        values: Vec::new(),
    };

    Some(delta_update)
}

// ====== SELF ======= //

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Subscribe {
    None,
    Some,
    All,
}
pub struct ActiveSubscriptions {
    pub mode: Subscribe,
    timeout: Duration,
    paths: HashMap<String, HashMap<ControlId, PathSubscribe>>,
}

impl ActiveSubscriptions {
    pub fn new(mode: Subscribe) -> ActiveSubscriptions {
        ActiveSubscriptions {
            mode,
            paths: HashMap::new(),
            timeout: Duration::from_secs(99999999),
        }
    }

    fn set_timeout(&mut self, timeout: u64) {
        if timeout < u64::MAX {
            let timeout = Duration::from_millis(timeout);
            if self.timeout < timeout {
                self.timeout = timeout;
            };
        }
    }

    pub fn get_timeout(&mut self) -> Duration {
        self.timeout
    }

    pub fn subscribe(&mut self, subscription: Subscription) -> Result<(), RadarError> {
        self.mode = Subscribe::Some;
        let mut period = u64::MAX;
        for path_subscription in subscription.subscribe {
            let (radar_id, control_id) = extract_path(&path_subscription.path);
            let mut paths = self.paths.get_mut(radar_id);
            if paths.is_none() {
                log::debug!("Creating radar '{}' self", radar_id);
                self.paths.insert(radar_id.to_string(), HashMap::new());
                paths = self.paths.get_mut(radar_id);
            }
            let paths = paths.unwrap();

            if control_id.contains("*") {
                for id in ControlId::iter() {
                    let matcher = WildMatch::new(control_id);
                    if matcher.matches(&id.to_string()) {
                        log::trace!("{} matches {}", id, control_id);
                        paths.insert(id, path_subscription.clone());
                    }
                }
                if let Some(p) = path_subscription.min_period {
                    period = min(p, period);
                }
                if let Some(p) = path_subscription.period {
                    period = min(p, period);
                }
            } else {
                match ControlId::from_str(control_id) {
                    Ok(control_id) => {
                        if let Some(p) = path_subscription.min_period {
                            period = min(p, period);
                        }
                        if let Some(p) = path_subscription.period {
                            period = min(p, period);
                        }
                        paths.insert(control_id, path_subscription);
                    }
                    Err(_e) => {
                        log::warn!(
                            "Cannot subscribe radar '{}' path '{}': does not exist",
                            radar_id,
                            control_id,
                        );
                        return Err(RadarError::CannotParseControlId(control_id.to_string()));
                    }
                }
            }
        }
        self.set_timeout(period);

        Ok(())
    }

    pub fn desubscribe(&mut self, subscription: Desubscription) -> Result<(), RadarError> {
        self.mode = Subscribe::Some;
        for path_desubscription in subscription.desubscribe {
            let (radar_id, control_id) = extract_path(&path_desubscription.path);
            let paths = self.paths.get_mut(radar_id);
            if paths.is_none() {
                continue;
            }
            let paths = paths.unwrap();

            if control_id.contains("*") {
                for id in ControlId::iter() {
                    let matcher = WildMatch::new(control_id);
                    if matcher.matches(&id.to_string()) {
                        paths.remove(&id);
                    }
                }
            } else {
                match ControlId::from_str(&control_id) {
                    Ok(id) => {
                        paths.remove(&id);
                    }
                    Err(_e) => {
                        log::warn!(
                            "Cannot desubscribe context '{}' path '{}': does not exist",
                            radar_id,
                            path_desubscription.path
                        );
                        return Err(RadarError::CannotParseControlId(control_id.to_string()));
                    }
                }
            }
        }

        Ok(())
    }

    //
    // This is called with a RadarControlValue generated internally, with a fixed path and no wildcards
    // and a control_id filled in.
    //
    pub fn is_subscribed(&mut self, rcv: &RadarControlValue, full: bool) -> bool {
        match self.mode {
            Subscribe::All => {
                return true;
            }
            Subscribe::None => {
                return false;
            }
            Subscribe::Some => {}
        }
        if let (Some(radar_id), Some(control_id)) = (rcv.radar_id.as_deref(), &rcv.control_id) {
            for key in [radar_id, "*"] {
                if let Some(paths) = self.paths.get_mut(key) {
                    if let Some(path) = paths.get_mut(control_id) {
                        let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);

                        if *policy == Policy::Fixed {
                            if !full {
                                return false;
                            }
                            if let Some(period) = path.period {
                                let now = SystemTime::now();

                                if path.last_sent.is_none()
                                    || path.last_sent.unwrap() + Duration::from_micros(period) > now
                                {
                                    path.last_sent = Some(now);
                                    return false;
                                }
                            }
                        }

                        if let Some(min_period) = path.min_period {
                            let now = SystemTime::now();

                            if path.last_sent.is_none()
                                || path.last_sent.unwrap() + Duration::from_micros(min_period) > now
                            {
                                path.last_sent = Some(now);
                                return false;
                            }
                        }
                        return true;
                    }
                }
            }
        } else {
            panic!("Invalid use of is_subscribed(), can only be done on internal RCV");
        }

        return false;
    }

    pub fn is_subscribed_path(&mut self, path: &str, full: bool) -> bool {
        match self.mode {
            Subscribe::All => {
                return true;
            }
            Subscribe::None => {
                return false;
            }
            Subscribe::Some => {}
        }
        let (radar_id, control_id) = extract_path(path);
        let control_id = match ControlId::from_str(control_id) {
            Ok(c) => c,
            Err(_) => {
                return false;
            }
        };

        for key in [radar_id, "*"] {
            if let Some(paths) = self.paths.get_mut(key) {
                if let Some(path) = paths.get_mut(&control_id) {
                    let policy = path.policy.as_ref().unwrap_or(&Policy::Instant);

                    if *policy == Policy::Fixed {
                        if !full {
                            return false;
                        }
                        if let Some(period) = path.period {
                            let now = SystemTime::now();

                            if path.last_sent.is_none()
                                || path.last_sent.unwrap() + Duration::from_micros(period) > now
                            {
                                path.last_sent = Some(now);
                                return false;
                            }
                        }
                    }

                    if let Some(min_period) = path.min_period {
                        let now = SystemTime::now();

                        if path.last_sent.is_none()
                            || path.last_sent.unwrap() + Duration::from_micros(min_period) > now
                        {
                            path.last_sent = Some(now);
                            return false;
                        }
                    }
                    return true;
                }
            }
        }

        return false;
    }
}

fn extract_path(mut path: &str) -> (&str, &str) {
    if path.starts_with("radars.") {
        path = &path["radars.".len()..];
    }
    if path == "*" {
        return ("*", "*");
    }
    if let Some((radar, mut control)) = path.split_once('.') {
        if control.starts_with("controls.") {
            control = &control["controls.".len()..];
        }
        return (radar, control);
    }

    ("*", path)
}

/// Client-to-server message to subscribe to control value updates
#[derive(Deserialize, Debug, Serialize, ToSchema)]
#[schema(example = json!({
    "subscribe": [
        {"path": "radars.*.controls.*", "period": 1000},
        {"path": "radars.nav1034A.controls.gain", "policy": "instant"}
    ]
}))]
pub struct Subscription {
    /// List of path subscriptions
    subscribe: Vec<PathSubscribe>,
}

/// Client-to-server message to unsubscribe from control value updates
#[derive(Deserialize, Debug, ToSchema)]
#[schema(example = json!({
    "desubscribe": [{"path": "radars.*.controls.gain"}]
}))]
pub struct Desubscription {
    /// List of paths to unsubscribe from
    desubscribe: Vec<PathSubscribe>,
}

/// A single path subscription specification
#[derive(Deserialize, Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PathSubscribe {
    /// Path pattern to subscribe to. Supports wildcards:
    /// - `radars.*.controls.*` - all controls on all radars
    /// - `radars.nav1034A.controls.gain` - specific control
    /// - `*.gain` - gain control on all radars
    #[schema(example = "radars.*.controls.*")]
    path: String,
    /// Update period in milliseconds (for fixed policy)
    #[schema(example = 1000)]
    period: Option<u64>,
    /// Delivery policy: instant (immediate), ideal (rate-limited), fixed (periodic)
    #[serde(default, deserialize_with = "deserialize_policy")]
    policy: Option<Policy>,
    /// Minimum period between updates in milliseconds
    #[schema(example = 200)]
    min_period: Option<u64>,
    #[serde(skip)]
    #[schema(ignore)]
    last_sent: Option<SystemTime>,
}

/// Subscription delivery policy
#[derive(Clone, Serialize, PartialEq, Debug, EnumString, VariantNames, ToSchema)]
#[strum(serialize_all = "camelCase")]
pub enum Policy {
    /// Send updates immediately when values change
    Instant,
    /// Rate-limit updates to minPeriod
    Ideal,
    /// Send updates at fixed intervals (period)
    Fixed,
}

use serde::Deserializer;

fn deserialize_policy<'de, D>(deserializer: D) -> Result<Option<Policy>, D::Error>
where
    D: Deserializer<'de>,
{
    // Try to read an Option<String>.  If the key is absent we get None.
    let opt = Option::<String>::deserialize(deserializer)?;

    match opt {
        Some(s) => Policy::from_str(&s.to_ascii_lowercase())
            .map(Some)
            .map_err(|_| serde::de::Error::unknown_variant(&s, &Policy::VARIANTS)),
        None => Ok(None), // field missing → None
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn deserialize_subscription() {
        let s = Subscription {
            subscribe: vec![
                PathSubscribe {
                    path: "radars.1.controls.gain".to_string(),
                    period: None,
                    policy: Some(Policy::Ideal),
                    min_period: Some(50),
                    last_sent: None,
                },
                PathSubscribe {
                    path: "radars.2.controls.gain".to_string(),
                    period: Some(1000),
                    policy: Some(Policy::Instant),
                    min_period: None,
                    last_sent: None,
                },
            ],
        };
        let r = serde_json::to_string(&s);
        assert!(r.is_ok());
        let r = r.unwrap();
        println!("r = {}", r);

        match serde_json::from_str::<Subscription>(&r) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.1.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Ideal));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{"subscribe":[{"path":"radars.1.controls.gain","period":null,"policy":"ideal","min_period":null}]}"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "radars.1.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Ideal));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "*.gain" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "*.gain");
                assert_eq!(r.subscribe[0].policy, None);
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "*" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "*");
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "radars.*.controls.gain" }, { "path": "radars.*.controls.power" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.controls.gain");
                assert_eq!(r.subscribe[1].path, "radars.*.controls.power");
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "radars.*.controls.gain", "policy": "instant", "period": 1000 }, { "path": "radars.*.controls.power", "period": 1000 } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.controls.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Instant));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }
    }
}
