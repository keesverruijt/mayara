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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr},
    str::FromStr,
};
use strum::EnumCount;
use tokio::sync::{
    broadcast::{self},
    mpsc,
};

use crate::web::spokes_handler;

use super::super::{Message, Web, WebSocket, WebSocketUpgrade};
use mayara::{
    radar::{Legend, RadarError, RadarInfo, SharedRadars},
    settings::{BareControlValue, Control, ControlId, ControlValue, RadarControlValue},
    stream::{ActiveSubscriptions, Desubscription, SignalKDelta, Subscribe, Subscription},
};

const PROVIDER: &str = mayara::PACKAGE;
const VERSION: &str = mayara::VERSION;
pub(crate) const BASE_URI: &str = "/signalk/v2/api/vessels/self/radars";
pub(crate) const CONTROL_URI: &str = "/signalk/v2/api/vessels/self/radars/stream";
pub(crate) const SPOKES_URI: &str = "/signalk/v2/api/vessels/self/radars/{id}/spokes"; // plus radar_id

pub(crate) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.add(get_radars())
        .add(get_interfaces())
        .route(CONTROL_URI, get(control_stream_handler))
        .route(SPOKES_URI, get(spokes_handler))
        .add(get_radar())
        .add(get_control_values())
        .add(get_control_value())
        .add(set_control_value())
        .add(openapi())
}

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars/resources/openapi.json",
    description = "OpenAPI spec"
)]
async fn openapi(State(_state): State<Web>) -> impl IntoResponse {
    // `build_openapi` caches the openapi spec, so it's not necessary to call it every time
    let openapi = build_openapi(|| {
        OpenApiBuilder::new().info(InfoBuilder::new().title(PROVIDER).version(VERSION))
    });

    Json(openapi)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApiV3 {
    name: String,
    brand: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    spoke_data_url: String,
    stream_url: String,
    radar_ip_address: Ipv4Addr,
}

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars",
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
        let spoke_data_uri = SPOKES_URI.replace("{id}", &info.key());
        let v = RadarApiV3 {
            name: info.controls.user_name(),
            brand: info.brand.to_string(),
            model: info.controls.model_name(),
            spoke_data_url: format!("ws://{}{}", host, spoke_data_uri),
            stream_url: format!("ws://{}{}", host, CONTROL_URI),
            radar_ip_address: *info.addr.ip(),
        };

        api.insert(info.key(), v);
    }
    wrap_response(api).into_response()
}

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars/interfaces",
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
        Some(api) => wrap_response(wrap("interfaces", api)).into_response(),
        _ => Json(Vec::<String>::new()).into_response(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
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
    controls: HashMap<ControlId, Control>,
}

impl Capabilities {
    fn new(info: RadarInfo, controls: HashMap<ControlId, Control>) -> Self {
        Capabilities {
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
            has_dual_radar: info.dual.is_some(),
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
            controls,
        }
    }
}

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/capabilities",
    description = "Get all static information about a specific radar"
)]
async fn get_radar(
    Path(radar_id): Path<String>,
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    log::debug!(
        "Radar capabilities request from {} for host '{}'",
        addr,
        host
    );

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.args.port
    );

    log::debug!("target host = '{}'", host);

    if let Some(info) = state.radars.get_by_key(&radar_id) {
        let controls = info.controls.get_controls();
        let v = Capabilities::new(info, controls);

        wrap_response(wrap(&radar_id, wrap("capabilities", v))).into_response()
    } else {
        Json(()).into_response()
    }
}

// =============================================================================
// Control Value REST API Handler
// =============================================================================

/// Parameters for control-specific endpoints
#[derive(Deserialize, ToSchema)]
#[allow(dead_code)] // Instantiation hidden in extractor
struct RadarControlIdParam {
    radar_id: String,
    control_id: String,
}

/// PUT /v2/api/vessels/self/radars/{radar_id}/controls/{control_id}
/// Sets a control value on the radar
#[endpoint(
    method = "PUT",
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}",
    description = "Set the value of a radar control"
)]
async fn set_control_value(
    Path(params): Path<RadarControlIdParam>,
    State(state): State<Web>,
    extract::Json(request): extract::Json<BareControlValue>,
) -> Response {
    let (radar_id, control_id) = (params.radar_id, params.control_id);
    log::debug!(
        "PUT control {} = {:?} for radar {}",
        control_id,
        request,
        radar_id
    );

    // Get the radar info and control without holding the lock across await
    let (controls, control_value) = {
        match state.radars.get_by_key(&radar_id) {
            Some(radar) => {
                // Look up the control by name
                let control = match radar.controls.get_by_id(&control_id) {
                    Some(c) => c,
                    None => {
                        // Debug: list all possible controls
                        let all = radar.controls.get_control_keys();
                        return (
                            StatusCode::BAD_REQUEST,
                            format!("Unknown control '{}' -- use {:?} instead", control_id, all),
                        )
                            .into_response();
                    }
                };

                let control_value = ControlValue::from_request(control.item().control_id, request);
                log::debug!("Map request to controlValue {:?}", control_value);
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
    if let Err(e) = controls.process_client_request(control_value, reply_tx) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
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

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}",
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

    match radars.get_by_key(&radar_id) {
        Some(radar) => {
            // Look up the control by name
            match radar.controls.get_by_id(&control_id) {
                Some(c) => {
                    let control_value = ControlValue::from(&c, None);
                    let response = wrap_response(wrap(
                        &radar_id,
                        wrap("controls", BareControlValue::from(control_value)),
                    ));

                    response.into_response()
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

//
// "version": "1.0.0",
//   "self": "urn:mrn:signalk:uuid:705f5f1a-efaf-44aa-9cb8-a0fd6305567c",
//   "vessels": {
//     "urn:mrn:signalk:uuid:705f5f1a-efaf-44aa-9cb8-a0fd6305567c": {
//       "navigation": {
//         "speedOverGround": {
//           "value": 4.32693662,
//

#[derive(Serialize, ToSchema)]
struct FullSignalKResponse {
    version: &'static str,
    radars: Value,
}

#[endpoint(
    method = "GET",
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls",
    description = "Get the value of all radar controls for a radar"
)]
#[axum::debug_handler]
async fn get_control_values(
    Path(radar_id): Path<String>,
    State(state): State<Web>,
) -> Result<Json<FullSignalKResponse>, RadarError> {
    log::debug!("GET radar {} controls", radar_id);

    match state.radars.get_by_key(&radar_id) {
        Some(radar) => Ok(wrap_response(get_controls(&radar))),
        None => Err(RadarError::NoSuchRadar(radar_id)),
    }
}

fn get_controls(info: &RadarInfo) -> Value {
    let rcvs = info.controls.get_radar_control_values();
    let full: serde_json::Map<String, Value> = rcvs
        .iter()
        .map(|rcv| {
            (
                rcv.control_id.unwrap().to_string(),
                serde_json::to_value(BareControlValue::from(rcv.clone())).unwrap(),
            )
        })
        .collect();

    wrap(&info.key(), wrap("controls", Value::Object(full)))
}

pub fn wrap<T>(outer: &str, value: T) -> Value
where
    T: Serialize,
{
    let value = serde_json::to_value(value).unwrap();
    let mut map = serde_json::Map::new();
    map.insert(outer.to_string(), value);
    Value::Object(map)
}

fn wrap_response<T>(value: T) -> Json<FullSignalKResponse>
where
    T: Serialize,
{
    Json(FullSignalKResponse {
        version: VERSION,
        radars: serde_json::to_value(value).unwrap(),
    })
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

async fn control_stream_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<SignalKWebSocket>,
    ws: WebSocketUpgrade,
) -> Response {
    log::info!(
        "stream request for \"/signalk/v2/api/vessels/self/radars/stream\" from {} params={:?}",
        addr,
        params
    );

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
    let mut meta_radar_data_sent = Vec::new();

    log::info!(
        "Starting /signalk/v2/api/vessels/self/radars/stream websocket subscribe={:?} send_cached_values={:?}",
        subscribe,
        send_cached_values
    );

    send_hello(&mut socket).await?;

    let mut subscriptions = ActiveSubscriptions::new(subscribe.clone());

    let mut sk_delta = SignalKDelta::new();
    sk_delta.add_meta_updates(&radars, &mut meta_radar_data_sent);

    if send_cached_values && subscribe == Subscribe::All {
        for radar in radars.get_active() {
            let rcvs: Vec<RadarControlValue> = radar.controls.get_radar_control_values();
            log::info!(
                "Sending {} controls for radar '{}'",
                rcvs.len(),
                radar.key()
            );

            sk_delta.add_updates(rcvs);
        }
    }

    if let Some(sk_delta) = sk_delta.build() {
        send_message(socket, sk_delta).await?;
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
                        if let Err(e) = send_message(socket, &message).await {
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
                    Ok(mut delta) => {
                        delta.apply_subscriptions(&mut subscriptions);
                        delta.add_meta_from_updates(&radars, &mut meta_radar_data_sent);

                        if let Some(sk_delta) = delta.build() {
                            send_message(socket, sk_delta).await?;
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
                                handle_client_request(&mut socket, message.as_str(), &mut subscriptions, &radars, reply_tx.clone()).await;
                            },
                            _ => {
                                log::debug!("Dropping unexpected message {:?}", message);
                            }
                        }

                    },
                    Some(Err(e)) => {
                        break map_axum_error(e);
                    },
                    None => {
                        // Stream has closed
                        log::debug!("Control websocket closed");
                        break Ok(());
                    }
                }
            }

            _ = tokio::time::sleep(subscriptions.get_timeout()) => {
                if let Err(e) = send_all_subscribed(&mut socket, &radars, &mut subscriptions).await
                {
                    log::warn!("Cannot send subscribed data to websocket");
                    break Err(e);
                }
            }
        }
    }
}

fn map_axum_error(e: axum::Error) -> Result<(), RadarError> {
    let msg = &format!("{:?}", e);
    log::debug!("Error reading websocket: {}", msg);
    if msg == "Protocol(ResetWithoutClosingHandshake)" {
        // Somebody pressed Ctrl-C in websocat, or client is likewise
        // careless in closing websocket
        return Ok(());
    }
    return Err(e.into());
}

async fn send_message<T>(socket: &mut WebSocket, message: T) -> Result<(), RadarError>
where
    T: Serialize,
{
    let message: String = serde_json::to_string(&message).unwrap();
    socket
        .send(Message::Text(message.into()))
        .await
        .map_err(|e| RadarError::Axum(e))?;
    Ok(())
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

async fn handle_client_request(
    socket: &mut WebSocket,
    message: &str,
    subscriptions: &mut ActiveSubscriptions,
    radars: &SharedRadars,
    reply_tx: mpsc::Sender<ControlValue>,
) {
    log::info!("Stream request: {}", message);

    let stream_request = serde_json::from_str::<StreamRequest>(message);

    log::info!("Decoded Stream request: {:?}", stream_request);

    if let Ok(stream_request) = stream_request {
        let r = match stream_request {
            StreamRequest::Subscription(subscription) => {
                handle_subscription(socket, radars, subscriptions, subscription).await
            }
            StreamRequest::Desubscription(desubscription) => {
                subscriptions.desubscribe(desubscription)
            }
            StreamRequest::RadarControlValue(rcv) => {
                handle_control_request(message, radars, reply_tx, rcv).await
            }
        };
        match r {
            Ok(()) => {}
            Err(e) => {
                let cv = BareControlValue::new_error(e.to_string());
                let str_message: String = serde_json::to_string(&cv).unwrap();
                log::debug!("stream error {}", str_message);
                let ws_message = Message::Text(str_message.into());

                let _ = socket.send(ws_message);
            }
        }
    }
}

async fn handle_control_request(
    message: &str,
    radars: &SharedRadars,
    reply_tx: mpsc::Sender<ControlValue>,
    mut rcv: RadarControlValue,
) -> Result<(), RadarError> {
    if let Some(radar_id) = rcv.parse_path() {
        if let Some(radar) = radars.get_by_key(&radar_id) {
            let control_value: ControlValue = rcv.into();
            radar
                .controls
                .process_client_request(control_value.clone(), reply_tx)
        } else {
            log::warn!(
                "No radar '{}' active; ControlValue '{}' ignored",
                radar_id,
                message
            );
            Err(RadarError::NoSuchRadar(radar_id.to_string()))
        }
    } else {
        log::warn!("Cannot determine control from path '{}'; ignored", rcv.path);
        Err(RadarError::CannotParseControlId(rcv.path))
    }
}

async fn handle_subscription(
    socket: &mut WebSocket,
    radars: &SharedRadars,
    subscriptions: &mut ActiveSubscriptions,
    subscription: Subscription,
) -> Result<(), RadarError> {
    subscriptions.subscribe(subscription)?;
    send_all_subscribed(socket, radars, subscriptions).await
}

async fn send_all_subscribed(
    socket: &mut WebSocket,
    radars: &SharedRadars,
    subscriptions: &mut ActiveSubscriptions,
) -> Result<(), RadarError> {
    let mut rcvs: Vec<RadarControlValue> = Vec::with_capacity(80);

    for radar in radars.get_active() {
        rcvs.append(&mut radar.controls.get_radar_control_values());
    }
    if subscriptions.mode == Subscribe::Some {
        rcvs.retain(|x| subscriptions.is_subscribed(x, true));
    }
    log::info!("Sending {} subscribed controls", rcvs.len());
    if rcvs.len() > 0 {
        let mut delta: SignalKDelta = SignalKDelta::new();
        delta.add_updates(rcvs);
        send_message(socket, delta.build().unwrap()).await?;
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
        name: PROVIDER,
        version: VERSION,
        timestamp: Utc::now(),
        roles: vec!["master"],
    };
    let message: String = serde_json::to_string(&message).unwrap();
    let ws_message = Message::Text(message.into());

    socket.send(ws_message).await
}
