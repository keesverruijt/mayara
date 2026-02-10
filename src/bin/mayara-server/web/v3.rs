use axum::{
    Error, Json,
    extract::{self, ConnectInfo, Path, Query, State},
    http::Uri,
    response::{IntoResponse, Response},
    routing::get,
};
use axum_openapi3::{
    AddRoute,      // `add` method for Router to add routes also to the openapi spec
    build_openapi, // function for building the openapi spec
    endpoint,      // function for cleaning the openapi cache (mostly used for testing)
    utoipa::{
        ToSchema,
        openapi::{InfoBuilder, OpenApiBuilder},
    },
};
use chrono::{DateTime, Utc};
use futures::SinkExt;
use http::StatusCode;
use hyper;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    cmp::min,
    collections::HashMap,
    net::SocketAddr,
    str::FromStr,
    time::{Duration, SystemTime},
};
use strum::{EnumCount, EnumString, IntoEnumIterator, VariantNames};
use tokio::sync::{
    broadcast::{self},
    mpsc,
};
use wildmatch::WildMatch;

use super::{Message, Web, WebSocket, WebSocketUpgrade};
use mayara::{
    radar::{Legend, RadarError, RadarInfo, SharedRadars},
    settings::{Control, ControlId, ControlValue, RadarControlValue},
};

pub(super) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.add(get_radars())
        .add(get_interfaces())
        .add(get_radar())
        .add(get_control_value())
        .add(set_control_value())
        .route("/v3/api/stream", get(stream_handler))
        .add(openapi())
}

#[endpoint(
    method = "GET",
    path = "/v3/api/resources/openapi.json",
    description = "OpenAPI spec"
)]
async fn openapi(State(_state): State<Web>) -> impl IntoResponse {
    // `build_openapi` caches the openapi spec, so it's not necessary to call it every time
    let openapi = build_openapi(|| {
        OpenApiBuilder::new().info(InfoBuilder::new().title("My Webserver").version("0.1.0"))
    });

    Json(openapi)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApiV3 {
    id: String,
    key: String,
    name: String,
    brand: String,
}

impl RadarApiV3 {
    fn new(id: String, key: String, name: String, brand: String) -> Self {
        RadarApiV3 {
            id,
            key,
            name,
            brand,
        }
    }
}

//
// Signal K radar API says this returns something like:
//    {"0":{"id":"0","name":"HALO","brand":"Navico","streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
#[endpoint(
    method = "GET",
    path = "/v3/api/radars",
    description = "Get all radars that have been detected and are online"
)]
async fn get_radars(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    log::debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.args.port
    );

    log::debug!("target host = '{}'", host);

    let mut api: HashMap<String, RadarApiV3> = HashMap::new();
    for info in state.radars.get_active().clone() {
        let id = format!("{}", info.id);
        let key = info.key();
        let name = info.controls.user_name();
        let brand = info.brand.to_string();
        let v = RadarApiV3::new(id.to_owned(), key, name, brand);

        api.insert(id.to_owned(), v);
    }
    Json(api).into_response()
}

#[endpoint(
    method = "GET",
    path = "/v3/api/interfaces",
    description = "Get information which network interfaces are usable by which radar brand"
)]
async fn get_interfaces(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    log::debug!("Interface state request from {} for host '{}'", addr, host);

    let (tx, mut rx) = mpsc::channel(1);
    state.tx_interface_request.send(Some(tx)).unwrap();
    match rx.recv().await {
        Some(api) => Json(api).into_response(),
        _ => Json(Vec::<String>::new()).into_response(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Characteristics {
    max_range: u32,
    min_range: u32,
    supported_ranges: Vec<u32>,
    spokes_per_revolution: u16,
    max_spoke_length: u16,
    pixel_values: u8,
    legend: Legend,
    has_doppler: bool,
    has_dual_range: bool,
    has_dual_radar: bool,
    no_transmit_sectors: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    id: String,
    name: String,
    stream_url: String,
    characteristics: Characteristics,
    controls: HashMap<ControlId, Control>,
}

impl Capabilities {
    fn new(
        id: String,
        name: String,
        stream_url: String,
        info: RadarInfo,
        controls: HashMap<ControlId, Control>,
    ) -> Self {
        let characteristics = Characteristics {
            max_range: info.ranges.all.last().map_or(0, |r| r.distance() as u32),
            min_range: info.ranges.all.first().map_or(0, |r| r.distance() as u32),
            supported_ranges: info
                .ranges
                .all
                .iter()
                .map(|r| r.distance() as u32)
                .collect(),
            spokes_per_revolution: info.spokes_per_revolution,
            max_spoke_length: info.max_spoke_len,
            pixel_values: info.pixel_values,
            legend: info.get_legend(),
            has_doppler: info.doppler,
            has_dual_range: info.dual_range,
            has_dual_radar: info.which.is_some(),
            no_transmit_sectors: controls
                .iter()
                .filter(|(ctype, _)| {
                    matches!(
                        ctype,
                        ControlId::NoTransmitStart1
                            | ControlId::NoTransmitStart2
                            | ControlId::NoTransmitStart3
                            | ControlId::NoTransmitStart4
                    )
                })
                .count() as u8,
        };
        Capabilities {
            id,
            name,
            stream_url,
            characteristics,
            controls,
        }
    }
}

#[endpoint(
    method = "GET",
    path = "/v3/api/radars/{radar_id}/capabilities",
    description = "Get all static information about a specific radar"
)]
async fn get_radar(
    Path(radar_id): Path<usize>,
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    log::debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.args.port
    );

    log::debug!("target host = '{}'", host);

    if let Some(info) = state.radars.get_by_id(radar_id) {
        let id = info.id.to_string();
        let stream_url = format!("ws://{}{}{}", host, super::v1::SPOKES_URI, id);
        let name = info.controls.user_name();

        if let Some(controls) = info.controls.get_controls() {
            let v = Capabilities::new(id.to_owned(), name, stream_url, info, controls);

            return Json(v).into_response();
        }
    }
    Json(()).into_response()
}

// =============================================================================
// Control Value REST API Handler
// =============================================================================

/// Parameters for control-specific endpoints
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // Instantiation hidden in extractor
struct RadarControlIdParam {
    radar_id: usize,
    control_id: String,
}

/// Request body for PUT /radars/{id}/controls/{control_id}
#[derive(Deserialize, Clone, Debug, ToSchema)]
#[allow(dead_code)] // Instantiation hidden in extractor
struct SetControlRequest {
    auto: Option<bool>,
    value: serde_json::Value,
}

/// PUT /v2/api/radars/{radar_id}/controls/{control_id}
/// Sets a control value on the radar
#[endpoint(
    method = "PUT",
    path = "/v3/api/radars/{radar_id}/controls/{control_id}",
    description = "Set the value of a radar control"
)]
async fn set_control_value(
    Path(params): Path<RadarControlIdParam>,
    State(state): State<Web>,
    extract::Json(request): extract::Json<SetControlRequest>,
) -> Response {
    let (radar_id, control_id) = (params.radar_id, params.control_id);
    log::debug!(
        "PUT control {} = {:?} for radar {}",
        control_id,
        request,
        radar_id
    );

    // Get the radar info and control  without holding the lock across await
    let (controls, control_value) = {
        let radars = state.radars.clone();

        match radars.get_by_id(radar_id) {
            Some(radar) => {
                // Look up the control by name
                let control = match radar.controls.get_by_id(&control_id) {
                    Some(c) => c,
                    None => {
                        // Debug: list all available controls
                        let available = radar.controls.get_control_keys();
                        log::warn!(
                            "Control '{}' not found. Available controls: {:?}",
                            control_id,
                            available
                        );
                        return (
                            StatusCode::BAD_REQUEST,
                            format!(
                                "Unknown control '{}' -- use {:?} instead",
                                control_id, available
                            ),
                        )
                            .into_response();
                    }
                };

                // Parse the value - handle compound controls {mode, value} and simple values
                let (value, auto) = (request.value.clone(), None);

                /* TODO!     match &request.value {                serde_json::Value::String(s) => {
                    // Try to normalize enum values using core definition
                    let normalized = if let Some(index) = control.enum_value_to_index(s) {
                        control
                            .index_to_enum_value(index)
                            .unwrap_or_else(|| s.clone())
                    } else {
                        s.clone()
                    };
                    log::debug!("Map request {:?} to string '{}'", request, normalized);
                    (normalized, None)
                }
                serde_json::Value::Number(n) => (n.to_string(), None),
                serde_json::Value::Bool(b) => (if *b { "1" } else { "0" }.to_string(), None),
                serde_json::Value::Object(obj) => {
                    // Check if this is a dopplerMode compound control {"enabled": bool, "mode": "target"|"rain"}
                    if control_id == "dopplerMode" {
                        let enabled = obj
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let mode_str =
                            obj.get("mode").and_then(|v| v.as_str()).unwrap_or("target");
                        // Convert mode string to numeric: "target" = 0, "rain" = 1
                        let mode_val = match mode_str {
                            "target" | "targets" => 0,
                            "rain" => 1,
                            _ => 0,
                        };
                        // Pass enabled state via 'auto' field (repurposed), mode as value
                        (mode_val.to_string(), Some(enabled))
                    } else {
                        // Standard compound control: {"mode": "auto"|"manual", "value": N}
                        let mode = obj.get("mode").and_then(|v| v.as_str()).unwrap_or("manual");
                        let auto = Some(mode == "auto");
                        let value = obj
                            .get("value")
                            .map(|v| match v {
                                serde_json::Value::Number(n) => n.to_string(),
                                serde_json::Value::String(s) => s.clone(),
                                _ => v.to_string(),
                            })
                            .unwrap_or_default();
                        (value, auto)
                    }
                }
                _ => (request.value.to_string(), None),
                */

                let mut control_value = ControlValue::new(control.item().control_id, value);
                control_value.auto = auto;
                log::debug!(
                    "Map request {:?} to controlValue {:?}",
                    request,
                    control_value
                );
                (radar.controls.clone(), control_value)
            }
            None => {
                return RadarError::NoSuchRadar(radar_id).into_response();
            }
        }
    };
    // Lock is released here

    // Create a channel for the reply
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);

    // Send the control request
    if let Err(e) = controls
        .process_client_request(control_value, reply_tx)
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to send control: {:?}", e),
        )
            .into_response();
    }

    // Wait briefly for a reply (error response)
    // Most controls don't reply on success, only on error
    tokio::select! {
        reply = reply_rx.recv() => {
            match reply {
                Some(cv) if cv.error.is_some() => {
                    return (StatusCode::BAD_REQUEST, cv.error.unwrap()).into_response();
                }
                _ => {}
            }
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
            // No error reply within timeout, assume success
        }
    }

    StatusCode::OK.into_response()
}

/// PUT /v2/api/radars/{radar_id}/controls/{control_id}
/// Sets a control value on the radar
#[endpoint(
    method = "GET",
    path = "/v3/api/radars/{radar_id}/controls/{control_id}",
    description = "Get the value of a radar control"
)]
async fn get_control_value(
    Path(params): Path<RadarControlIdParam>,
    State(state): State<Web>,
) -> Response {
    let (radar_id, control_id) = (params.radar_id, params.control_id);
    log::debug!("GET radar {} control {}", radar_id, control_id,);

    // Get the radar info and control  without holding the lock across await
    let radars = state.radars;

    match radars.get_by_id(radar_id) {
        Some(radar) => {
            // Look up the control by name
            match radar.controls.get_by_id(&control_id) {
                Some(c) => {
                    let control_value = ControlValue::from(&c, None);
                    Json(control_value).into_response()
                }
                None => {
                    // Debug: list all available controls
                    let available = radar.controls.get_control_keys();
                    log::warn!(
                        "Control '{}' not found. Available controls: {:?}",
                        control_id,
                        available
                    );
                    (
                        StatusCode::BAD_REQUEST,
                        format!(
                            "Unknown control '{}' -- use {:?} instead",
                            control_id, available
                        ),
                    )
                        .into_response()
                }
            }
        }
        None => RadarError::NoSuchRadar(radar_id).into_response(),
    }
}

///
/// Stream handler implementing the Signal K Stream procotol
///

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SignalKWebSocket {
    subscribe: Option<String>,
    send_cached_values: Option<String>,
}

async fn stream_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<SignalKWebSocket>,
    ws: WebSocketUpgrade,
) -> Response {
    log::info!("stream request from {} params={:?}", addr, params);

    let subscribe = match params.subscribe.as_deref() {
        None | Some("self") | Some("all") => Subscribe::All,
        Some("none") => Subscribe::None,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "Unknown subscribe value '{}' -- use 'none', 'self' or 'all' instead",
                    params.subscribe.unwrap()
                ),
            )
                .into_response();
        }
    };
    let send_cached_values = match params.send_cached_values.as_deref() {
        None | Some("true") => true,
        Some("false") => false,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "Unknown sendCachedValues value '{}' -- use 'false' or 'true' instead",
                    params.send_cached_values.unwrap()
                ),
            )
                .into_response();
        }
    };

    let ws = ws.accept_compression(true);

    let radars = state.radars.clone();
    let shutdown_tx = state.shutdown_tx.clone();

    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| {
        ws_signalk_delta_shim(socket, subscribe, send_cached_values, radars, shutdown_tx)
    })
}

async fn ws_signalk_delta_shim(
    mut socket: WebSocket,
    subscribe: Subscribe,
    send_cached_values: bool,
    radars: SharedRadars,
    shutdown_tx: broadcast::Sender<()>,
) {
    if let Err(e) = ws_signalk_delta(
        &mut socket,
        subscribe,
        send_cached_values,
        radars,
        shutdown_tx,
    )
    .await
    {
        log::error!("SignalK stream error: {e}");
    }
    let _ = socket.close().await;
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Subscribe {
    None,
    Some,
    All,
}

/// Actual websocket statemachine (one will be spawned per connection)
/// This needs to handle the (complex) Signal K state, which can request data from multiple
/// radars using a single websocket
///
async fn ws_signalk_delta(
    mut socket: &mut WebSocket,
    subscribe: Subscribe,
    send_cached_values: bool,
    radars: SharedRadars,
    shutdown_tx: broadcast::Sender<()>,
) -> Result<(), RadarError> {
    let mut broadcast_control_rx = radars.new_sk_client_subscription();
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel::<ControlValue>(ControlId::COUNT);

    log::info!(
        "Starting /stream v3 websocket subscribe={:?} send_cached_values={:?}",
        subscribe,
        send_cached_values
    );

    send_hello(&mut socket).await?;

    let mut subscriptions = ActiveSubscriptions::new(subscribe.clone());

    if send_cached_values && subscribe == Subscribe::All {
        for radar in radars.get_active() {
            let rcvs: Vec<RadarControlValue> = radar.controls.get_radar_control_values();
            log::info!(
                "Sending {} controls for radar '{}'",
                rcvs.len(),
                radar.key()
            );

            let message: SignalKDelta = rcvs.into();
            let message: String = serde_json::to_string(&message).unwrap();
            socket
                .send(Message::Text(message.into()))
                .await
                .map_err(|e| RadarError::Axum(e))?;
        }
    }

    loop {
        let mut shutdown_rx = shutdown_tx.subscribe();

        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::debug!("Shutdown of /stream websocket");
                break Ok(());
            },

            // this is where we receive directed control messages meant just for us, they
            // are either error replies for an invalid control value or the full list of
            // controls.
            r = reply_rx.recv() => {
                match r {
                    Some(message) => {
                        let str_message: String = serde_json::to_string(&message).unwrap();
                        log::debug!("/control serialize {:?} as {}", message, str_message);
                        let ws_message = Message::Text(str_message.into());

                        if let Err(e) = socket.send(ws_message).await {
                            log::error!("send to websocket client: {e}");
                            break Err(e.into());
                        }

                    },
                    None => {
                        log::error!("Error on Control channel");
                        break Err(RadarError::NotConnected);
                    }
                }
            },
            r = broadcast_control_rx.recv() => {
                match r {
                    Ok(rcv) => {
                        if is_subscribed(&rcv, &mut subscriptions, false) {
                            let rcv = vec![rcv];
                            let message: SignalKDelta = rcv.into();
                            let message: String = serde_json::to_string(&message).unwrap();
                            let ws_message = Message::Text(message.into());

                            if let Err(e) = socket.send(ws_message).await {
                                log::error!("send to websocket client: {e}");
                                break Err(e.into());
                            }
                        }
                    },
                    Err(e) => {
                        log::error!("Error on Control channel: {e}");
                        break Ok(());
                    }
                }
            },

            // receive control values from the client
            r = socket.recv() => {
                log::info!("Receiving {:?}", r);
                match r {
                    Some(Ok(message)) => {
                        match message {
                            Message::Text(message) => {
                                handle_client_request(message.as_str(), &mut subscriptions, &radars, reply_tx.clone()).await;

                            },
                            _ => {
                                log::debug!("Dropping unexpected message {:?}", message);
                            }
                        }

                    },
                    Some(Err(e)) => {
                        log::error!("Error reading websocket: {:?}", e);
                        break Err(e.into());
                    },
                    None => {
                        // Stream has closed
                        log::debug!("Control websocket closed");
                        break Ok(());
                    }
                }
            }

            _ = tokio::time::sleep(subscriptions.timeout) => {
                if let Err(e) = send_all_subscribed(&mut socket, &radars, &mut subscriptions).await
                {
                    log::warn!("Cannot send subscribed data to websocket");
                    break Err(e);
                }
            }
        }
    }
}

#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum StreamRequest {
    RadarControlValue(RadarControlValue),
    Subscription(Subscription),
    Desubscription(Desubscription),
}

//
// {
//   "context": "vessels.self",
//   "subscribe": [
//     {
//       "path": "radars.<id>.gain",
//       "period": 1000,
//       "format": "delta",
//       "policy": "ideal",
//       "minPeriod": 200
//     },
//     {
//       "path": "*.sea",
//       "period": 2000
//     },
//     {
//       "path": "radars.<id>.*",
//       "period": 2000
//     },
//     {
//       "path": "*",
//       "period": 10000
//     }
//   ]
// }
//
#[derive(Deserialize, Debug, Serialize)]
struct Subscription {
    subscribe: Vec<PathSubscribe>,
}

#[derive(Deserialize, Debug)]
struct Desubscription {
    desubscribe: Vec<PathSubscribe>,
}

#[derive(Deserialize, Debug, Clone, Serialize)]
#[serde(rename = "camelCase")]
struct PathSubscribe {
    path: String,
    period: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_policy")]
    policy: Option<Policy>,
    min_period: Option<u64>,
    #[serde(skip)]
    last_sent: Option<SystemTime>,
}

#[derive(Clone, Serialize, PartialEq, Debug, EnumString, VariantNames)]
#[strum(serialize_all = "camelCase")]
enum Policy {
    Instant,
    Ideal,
    Fixed,
}

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

struct ActiveSubscriptions {
    mode: Subscribe,
    timeout: Duration,
    paths: HashMap<String, HashMap<ControlId, PathSubscribe>>,
}

impl ActiveSubscriptions {
    fn new(mode: Subscribe) -> ActiveSubscriptions {
        ActiveSubscriptions {
            mode,
            paths: HashMap::new(),
            timeout: Duration::from_secs(60),
        }
    }
}

impl ActiveSubscriptions {
    fn set_timeout(&mut self, timeout: u64) {
        if timeout < u64::MAX {
            let timeout = Duration::from_millis(timeout);
            if self.timeout < timeout {
                self.timeout = timeout;
            };
        }
    }
}

async fn handle_client_request(
    message: &str,
    subscriptions: &mut ActiveSubscriptions,
    radars: &SharedRadars,
    reply_tx: mpsc::Sender<ControlValue>,
) {
    log::info!("Stream request: {}", message);

    let stream_request = serde_json::from_str::<StreamRequest>(message);

    log::info!("Decoded Stream request: {:?}", stream_request);

    if let Ok(stream_request) = stream_request {
        match stream_request {
            StreamRequest::Subscription(subscription) => {
                handle_subscription(subscriptions, subscription);
            }
            StreamRequest::Desubscription(desubscription) => {
                handle_desubscription(subscriptions, desubscription);
            }
            StreamRequest::RadarControlValue(rcv) => {
                handle_control_request(message, radars, reply_tx, rcv).await;
            }
        }
    }
}

async fn handle_control_request(
    message: &str,
    radars: &SharedRadars,
    reply_tx: mpsc::Sender<ControlValue>,
    mut rcv: RadarControlValue,
) {
    if let Some(radar_id) = rcv.parse_path() {
        if let Some(radar) = radars.get_by_key(&radar_id) {
            let control_value = rcv.into();
            match radar
                .controls
                .process_client_request(control_value, reply_tx)
                .await
            {
                Ok(()) => {
                    log::debug!("ControlValue {} handled", message);
                }
                Err(e) => {
                    log::warn!("ControlValue {} error: {}", message, e);
                }
            }
        } else {
            log::warn!(
                "No radar '{}' active; ControlValue '{}' ignored",
                radar_id,
                message
            );
        }
    } else {
        log::warn!("Cannot determine control from path '{}'; ignored", rcv.path);
        return;
    }
}

fn handle_subscription(subscriptions: &mut ActiveSubscriptions, subscription: Subscription) {
    subscriptions.mode = Subscribe::Some;

    let mut period = u64::MAX;
    for path_subscription in subscription.subscribe {
        let (radar_id, control_id) = extract_path(&path_subscription.path);
        let mut paths = subscriptions.paths.get_mut(radar_id);
        if paths.is_none() {
            log::info!("Creating radar '{}' subscriptions", radar_id);
            subscriptions
                .paths
                .insert(radar_id.to_string(), HashMap::new());
            paths = subscriptions.paths.get_mut(radar_id);
        }
        let paths = paths.unwrap();

        if control_id.contains("\\*") {
            for id in ControlId::iter() {
                let matcher = WildMatch::new(control_id);
                if matcher.matches(&id.to_string()) {
                    log::info!("{} matches {}", id, control_id);
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
                }
            }
        }
    }
    subscriptions.set_timeout(period);
}

fn handle_desubscription(subscriptions: &mut ActiveSubscriptions, subscription: Desubscription) {
    subscriptions.mode = Subscribe::Some;
    for path_desubscription in subscription.desubscribe {
        let (radar_id, control_id) = extract_path(&path_desubscription.path);
        let paths = subscriptions.paths.get_mut(radar_id);
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
                }
            }
        }
    }
}

fn extract_path(mut path: &str) -> (&str, &str) {
    if path.starts_with("radars.") {
        path = &path["radars.".len()..];
    }
    if path == "*" {
        return ("*", "*");
    }
    if let Some(r) = path.split_once('.') {
        return r;
    }

    ("*", path)
}

//
// This is called with a RadarControlValue generated internally, with a fixed path and no wildcards
// and a control_id filled in.
fn is_subscribed(
    rcv: &RadarControlValue,
    subscriptions: &mut ActiveSubscriptions,
    full: bool,
) -> bool {
    match subscriptions.mode {
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
            if let Some(paths) = subscriptions.paths.get_mut(key) {
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

async fn send_all_subscribed(
    socket: &mut WebSocket,
    radars: &SharedRadars,
    subscriptions: &mut ActiveSubscriptions,
) -> Result<(), RadarError> {
    for radar in radars.get_active() {
        let mut rcvs: Vec<RadarControlValue> = radar.controls.get_radar_control_values();
        log::info!(
            "Sending {} controls for radar {} label {}",
            rcvs.len(),
            radar.id,
            radar.key()
        );
        if subscriptions.mode == Subscribe::Some {
            rcvs.retain(|x| is_subscribed(x, subscriptions, true));
        }

        let message: SignalKDelta = rcvs.into();
        let message: String = serde_json::to_string(&message).unwrap();
        socket
            .send(Message::Text(message.into()))
            .await
            .map_err(|e| RadarError::Axum(e))?;
    }
    Ok(())
}

#[derive(Serialize)]
struct SignalKHello {
    name: &'static str,
    version: &'static str,
    #[serde(serialize_with = "to_rfc3339")]
    timestamp: DateTime<Utc>,
    roles: Vec<&'static str>,
}

// Helper that turns a `DateTime` into an RFC‑3339 string when serializing
fn to_rfc3339<S>(dt: &DateTime<Utc>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&dt.to_rfc3339())
}

async fn send_hello(socket: &mut WebSocket) -> Result<(), Error> {
    let message = SignalKHello {
        name: "Marine Yacht Radar",
        version: crate::mayara::VERSION,
        timestamp: Utc::now(),
        roles: vec!["master"],
    };
    let message: String = serde_json::to_string(&message).unwrap();
    let ws_message = Message::Text(message.into());

    socket.send(ws_message).await
}

// A typical delta from SK:
//
// {
//   "context": "vessels.urn:mrn:imo:mmsi:234567890",
//   "updates": [
//     {
//       "source": {
//         "label": "N2000-01",
//         "type": "NMEA2000",
//         "src": "115",
//         "pgn": 128275
//       },
//       "values": [
//         {
//           "path": "navigation.trip.log",
//           "value": 43374
//         },
//         {
//           "path": "navigation.log",
//           "value": 17404540
//         }
//       ]
//     }
//   ]
// }

#[derive(Serialize)]
struct SignalKDelta {
    context: &'static str,
    updates: Vec<DeltaUpdate>,
}

#[derive(Serialize)]
struct DeltaUpdate {
    source: Source,
    values: Vec<DeltaValue>,
}

#[derive(Serialize)]
struct Source {
    label: String,
    src: &'static str,
    r#type: &'static str,
}

#[derive(Serialize)]
struct DeltaValue {
    path: String,
    value: serde_json::Value,
}

impl From<Vec<RadarControlValue>> for SignalKDelta {
    fn from(radar_control_values: Vec<RadarControlValue>) -> Self {
        let radar_id = radar_control_values[0].radar_id.clone().unwrap();

        let mut values = Vec::new();
        for radar_control_value in radar_control_values {
            let path = radar_control_value.path;

            let value = radar_control_value.value;
            values.push(DeltaValue { path, value });
        }

        let context = "self";
        let delta_update = DeltaUpdate {
            source: Source {
                label: radar_id,
                src: "mayara",
                r#type: "radar",
            },
            values,
        };

        let updates = vec![delta_update];
        SignalKDelta { context, updates }
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
                    path: "radars.1.gain".to_string(),
                    period: None,
                    policy: Some(Policy::Ideal),
                    min_period: Some(50),
                    last_sent: None,
                },
                PathSubscribe {
                    path: "radars.2.gain".to_string(),
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
                assert_eq!(r.subscribe[0].path, "radars.1.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Ideal));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{"subscribe":[{"path":"radars.1.gain","period":null,"policy":"ideal","min_period":null}]}"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 1);
                assert_eq!(r.subscribe[0].path, "radars.1.gain");
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

        let s = r#"{ "subscribe": [ { "path": "radars.*.gain" }, { "path": "radars.*.power" } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.gain");
                assert_eq!(r.subscribe[1].path, "radars.*.power");
            }
            Err(e) => {
                panic!("{}", e);
            }
        }

        let s = r#"{ "subscribe": [ { "path": "radars.*.gain", "policy": "instant", "period": 1000 }, { "path": "radars.*.power", "period": 1000 } ] }"#;
        match serde_json::from_str::<Subscription>(s) {
            Ok(r) => {
                assert_eq!(r.subscribe.len(), 2);
                assert_eq!(r.subscribe[0].path, "radars.*.gain");
                assert_eq!(r.subscribe[0].policy, Some(Policy::Instant));
            }
            Err(e) => {
                panic!("{}", e);
            }
        }
    }
}
