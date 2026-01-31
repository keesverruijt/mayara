use axum::{
    Json,
    extract::{self, ConnectInfo, Path, State},
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
use http::StatusCode;
use hyper;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr, str::FromStr};
use strum::EnumCount;
use tokio::sync::{
    broadcast::{self},
    mpsc,
};

use super::{Message, Web, WebSocket, WebSocketUpgrade};
use mayara::{
    radar::{Legend, RadarError, RadarInfo, SharedRadars},
    settings::{Control, ControlType, ControlValue, RadarControlValue},
};

pub(super) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.add(get_radars())
        .add(get_interfaces())
        .add(get_radar())
        .add(set_control_value())
        .route("/v3/api/stream", get(stream_handler))
        .add(openapi())
}

#[endpoint(
    method = "GET",
    path = "/v3/api/openapi.json",
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
    name: String,
    brand: String,
    stream_url: String,
}

impl RadarApiV3 {
    fn new(id: String, name: String, brand: String, stream_url: String) -> Self {
        RadarApiV3 {
            id,
            name,
            brand,
            stream_url,
        }
    }
}

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"HALO","brand":"Navico","streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
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
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}{}{}", host, super::v1::SPOKES_URI, id);
        let name = info.controls.user_name();
        let brand = info.brand.to_string();
        let v = RadarApiV3::new(id.to_owned(), name, brand, stream_url);

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
    controls: HashMap<ControlType, Control>,
}

impl Capabilities {
    fn new(
        id: String,
        name: String,
        stream_url: String,
        info: RadarInfo,
        controls: HashMap<ControlType, Control>,
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
            legend: info.legend.clone(),
            has_doppler: info.doppler,
            has_dual_range: info.dual_range,
            has_dual_radar: info.which.is_some(),
            no_transmit_sectors: controls
                .iter()
                .filter(|(ctype, _)| {
                    matches!(
                        ctype,
                        ControlType::NoTransmitStart1
                            | ControlType::NoTransmitStart2
                            | ControlType::NoTransmitStart3
                            | ControlType::NoTransmitStart4
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
    Path(radar_id): Path<String>,
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

    if let Some(info) = state.radars.get_by_id(&radar_id).clone() {
        let id = format!("radar-{}", info.id);
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
    radar_id: String,
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

    // Get the radar info and control type without holding the lock across await
    let (controls, control_type) = {
        let radars = state.radars.clone();

        match radars.get_by_id(&radar_id) {
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
                                "Unknown control: {} -- use {:?} instead",
                                control_id, available
                            ),
                        )
                            .into_response();
                    }
                };

                // Parse the value - handle compound controls {mode, value} and simple values
                let (value_str, auto) = match &request.value {
                    serde_json::Value::String(s) => {
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
                };

                let mut control_value = ControlValue::new(control.item().control_type, value_str);
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
        .process_client_request(control_type, reply_tx)
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

#[derive(Deserialize)]
struct SignalKWebSocket {
    subscribe: Option<String>,
}

async fn stream_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<SignalKWebSocket>,
    ws: WebSocketUpgrade,
) -> Response {
    log::debug!(
        "stream request from {} subscribe={:?}",
        addr,
        params.subscribe
    );

    let ws = ws.accept_compression(true);

    let radars = state.radars.clone();
    let shutdown_tx = state.shutdown_tx.clone();

    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| ws_signalk_delta(socket, params, radars, shutdown_tx))
}

/// Actual websocket statemachine (one will be spawned per connection)
/// This needs to handle the (complex) Signal K state, which can request data from multiple
/// radars using a single websocket
///
async fn ws_signalk_delta(
    mut socket: WebSocket,
    params: SignalKWebSocket,
    radars: SharedRadars,
    shutdown_tx: broadcast::Sender<()>,
) {
    let mut broadcast_control_rx = radars.new_sk_client_subscription();

    let (reply_tx, _reply_rx) = tokio::sync::mpsc::channel(4 * ControlType::COUNT);

    log::debug!("Starting /stream v3 websocket");

    if let Some(subscribe) = params.subscribe {
        if subscribe != "none" {
            for radar in radars.get_active() {
                if radar
                    .controls
                    .send_all_controls(reply_tx.clone())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }

    loop {
        let mut shutdown_rx = shutdown_tx.subscribe();

        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::debug!("Shutdown of /stream websocket");
                break;
            },

            r = broadcast_control_rx.recv() => {
                match r {
                    Ok(message) => {

                        let message: SignalKDelta = message.into();
                        let message: String = serde_json::to_string(&message).unwrap();
                        let ws_message = Message::Text(message.into());

                        if let Err(e) = socket.send(ws_message).await {
                            log::error!("send to websocket client: {e}");
                            break;
                        }


                    },
                    Err(e) => {
                        log::error!("Error on Control channel: {e}");
                        break;
                    }
                }
            },

            // receive control values from the client
            r = socket.recv() => {
                match r {
                    Some(Ok(message)) => {
                        match message {
                            Message::Text(message) => {
                                // Handle Signal K subscribe, unsubscribe, etc.

                            },
                            _ => {
                                log::debug!("Dropping unexpected message {:?}", message);
                            }
                        }

                    },
                    None => {
                        // Stream has closed
                        log::debug!("Control websocket closed");
                        break;
                    }
                    r => {
                        log::error!("Error reading websocket: {:?}", r);
                        break;
                    }
                }
            }
        }
    }
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
struct SignalKDelta<'a> {
    context: &'a str,
    updates: Vec<DeltaUpdate<'a>>,
}

#[derive(Serialize)]
struct DeltaUpdate<'a> {
    source: Source<'a>,
    values: Vec<DeltaValue>,
}

#[derive(Serialize)]
struct Source<'a> {
    label: String,
    r#type: &'a str,
}

#[derive(Serialize)]
struct DeltaValue {
    path: String,
    value: serde_json::Value,
}

impl From<RadarControlValue> for SignalKDelta<'_> {
    fn from(radar_control_value: RadarControlValue) -> Self {
        let path = radar_control_value.control_value.id.to_string();
        let mut values = Vec::new();
        if let Some(auto) = radar_control_value.control_value.auto {
            let path = path.clone() + "Auto";
            let value = serde_json::Value::Bool(auto);
            values.push(DeltaValue { path, value });
        }
        let value = serde_json::Value::String(radar_control_value.control_value.value);
        values.push(DeltaValue { path, value });

        let context = "self";
        let delta_update = DeltaUpdate {
            source: Source {
                label: radar_control_value.radar_id,
                r#type: "mayara",
            },
            values,
        };

        let updates = vec![delta_update];
        SignalKDelta { context, updates }
    }
}
