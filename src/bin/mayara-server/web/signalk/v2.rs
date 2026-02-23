use axum::{
    Error, Json,
    extract::{self, ConnectInfo, Path, Query, State},
    http::Uri,
    response::{IntoResponse, Response},
    routing::get,
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
use utoipa::OpenApi;
use utoipa::ToSchema;
use utoipa_swagger_ui::{Config as SwaggerConfig, SwaggerUi};

use crate::web::spokes_handler;

use super::super::{Message, Web, WebSocket, WebSocketUpgrade};
use mayara::{
    InterfaceApi,
    radar::{
        Legend, RadarError, RadarInfo, SharedRadars,
        settings::{BareControlValue, Control, ControlId, ControlValue, RadarControlValue},
    },
    stream::{ActiveSubscriptions, Desubscription, SignalKDelta, Subscribe, Subscription},
};

const PROVIDER: &str = mayara::PACKAGE;
const VERSION: &str = mayara::VERSION;
pub(crate) const BASE_URI: &str = "/signalk/v2/api/vessels/self/radars";
pub(crate) const CONTROL_URI: &str = "/signalk/v1/stream";
pub(crate) const SPOKES_URI: &str = "/signalk/v2/api/vessels/self/radars/{id}/spokes"; // plus radar_id
const OPENAPI_URI: &str = "/signalk/v2/api/vessels/self/radars/resources/openapi.json";
const RADAR_CAPABILITIES_URI: &str = "/signalk/v2/api/vessels/self/radars/{radar_id}/capabilities";
const INTERFACES_URI: &str = "/signalk/v2/api/vessels/self/radars/interfaces";
const RADAR_CONTROLS_URI: &str = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls";
const RADAR_CONTROL_URI: &str =
    "/signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}";

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Mayara Radar API",
        version = "2.0.0",
        description = "REST API for controlling marine radars. Supports Navico (Simrad, B&G, Lowrance), \
                       Furuno, and Raymarine radar systems. Provides endpoints for discovering radars, \
                       reading and setting control values, and accessing radar data via WebSocket streams."
    ),
    tags(
        (name = "Radars", description = "Radar discovery and capabilities"),
        (name = "Controls", description = "Read and modify radar control settings"),
        (name = "Configuration", description = "Server and network configuration"),
        (name = "Stream", description = "Real-time WebSocket stream for control updates")
    ),
    paths(
        get_radars,
        get_interfaces,
        get_radar,
        get_control_values,
        get_control_value,
        set_control_value,
        control_stream_docs,
    ),
    components(schemas(
        RadarControlIdParam,
        FullSignalKResponse,
        RadarsResponse,
        RadarApiV3,
        RadarInterfaces,
        Interfaces,
        Capabilities,
        BareControlValue,
        // WebSocket message types
        SignalKDelta,
        Subscription,
        Desubscription,
        RadarControlValue,
    ))
)]
struct ApiDoc;

pub(crate) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.route(BASE_URI, get(get_radars))
        .route(INTERFACES_URI, get(get_interfaces))
        .route(CONTROL_URI, get(control_stream_handler))
        .route(SPOKES_URI, get(spokes_handler))
        .route(RADAR_CAPABILITIES_URI, get(get_radar))
        .route(RADAR_CONTROLS_URI, get(get_control_values))
        .route(
            RADAR_CONTROL_URI,
            get(get_control_value).put(set_control_value),
        )
        .route(OPENAPI_URI, get(openapi_json))
        .merge(SwaggerUi::new("/swagger-ui").config(SwaggerConfig::new([OPENAPI_URI])))
}

async fn openapi_json() -> impl IntoResponse {
    let spec = ApiDoc::openapi();
    let json = serde_json::to_string_pretty(&spec).unwrap();
    (
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json,
    )
}

/// Generate the OpenAPI specification as a JSON string
pub fn generate_openapi_json() -> String {
    let spec = ApiDoc::openapi();
    serde_json::to_string_pretty(&spec).unwrap()
}

/// Information about a detected radar, including WebSocket URLs for data streams
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(example = json!({
    "name": "HALO 034A",
    "brand": "Navico",
    "model": "HALO",
    "spokeDataUrl": "ws://192.168.1.100:8080/signalk/v2/api/vessels/self/radars/nav1034A/spokes",
    "streamUrl": "ws://192.168.1.100:8080/signalk/v1/stream",
    "radarIpAddress": "192.168.1.50"
}))]
struct RadarApiV3 {
    /// User-defined name or auto-detected model name
    #[schema(example = "HALO 034A")]
    name: String,
    /// Radar manufacturer brand (Navico, Furuno, Raymarine, Garmin)
    #[schema(example = "Navico")]
    brand: String,
    /// Radar model name if detected
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(example = "HALO")]
    model: Option<String>,
    /// WebSocket URL for receiving raw radar spoke data (binary)
    #[schema(
        example = "ws://192.168.1.100:8080/signalk/v2/api/vessels/self/radars/nav1034A/spokes"
    )]
    spoke_data_url: String,
    /// WebSocket URL for Signal K control stream (JSON)
    #[schema(example = "ws://192.168.1.100:8080/signalk/v1/stream")]
    stream_url: String,
    /// IP address of the radar unit on the network
    #[schema(value_type = String, example = "192.168.1.50")]
    radar_ip_address: Ipv4Addr,
}

/// Response containing all active radars keyed by radar ID
#[derive(Serialize, ToSchema)]
#[schema(example = json!({
    "version": "3.0.0",
    "radars": {
        "nav1034A": {
            "name": "HALO 034A",
            "brand": "Navico",
            "model": "HALO",
            "spokeDataUrl": "ws://192.168.1.100:8080/signalk/v2/api/vessels/self/radars/nav1034A/spokes",
            "streamUrl": "ws://192.168.1.100:8080/signalk/v1/stream",
            "radarIpAddress": "192.168.1.50"
        }
    }
}))]
struct RadarsResponse {
    version: &'static str,
    #[schema(value_type = HashMap<String, RadarApiV3>)]
    radars: Value,
}

#[utoipa::path(
    get,
    path = "/signalk/v2/api/vessels/self/radars",
    summary = "List all active radars",
    description = "Returns all radars that have been detected on the network and are currently online. \
                   Each radar entry includes WebSocket URLs for accessing spoke data and control streams.",
    responses(
        (status = 200, body = RadarsResponse, description = "Map of radar IDs to radar information")
    ),
    tag = "Radars"
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

/// Wrapper for radar interface information following Signal K structure
#[derive(Serialize, ToSchema)]
#[schema(example = json!({
    "radars": {
        "interfaces": {
            "brands": ["Navico", "Furuno", "Raymarine"],
            "interfaces": {
                "en0": {
                    "ip": "192.168.1.100",
                    "netmask": "255.255.255.0",
                    "listeners": {
                        "Furuno": "No match for 172.31.255.255",
                        "Navico": "Active",
                        "Raymarine": "Listening"
                    }
                }
            }
        }
    }
}))]
struct RadarInterfaces {
    radars: Interfaces,
}

/// Container for interface API data
#[derive(Serialize, ToSchema)]
struct Interfaces {
    interfaces: InterfaceApi,
}

#[utoipa::path(
    get,
    path = "/signalk/v2/api/vessels/self/radars/interfaces",
    summary = "List network interfaces",
    description = "Returns information about which network interfaces are available and which radar brands \
                   are listening on each interface. Useful for diagnosing network configuration issues.",
    responses(
        (status = 200, body = RadarInterfaces, description = "Network interface status for each radar brand")
    ),
    tag = "Configuration"
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
        Some(api) => wrap_response(Interfaces { interfaces: api }).into_response(),
        _ => Json(Vec::<String>::new()).into_response(),
    }
}

/// Static capabilities and configuration of a radar unit
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(example = json!({
    "maxRange": 74080,
    "minRange": 50,
    "supportedRanges": [50, 75, 100, 250, 500, 750, 1000, 1500, 2000, 3000, 4000, 6000, 8000, 12000, 16000, 24000, 36000, 48000, 64000, 74080],
    "spokesPerRevolution": 2048,
    "maxSpokeLength": 1024,
    "pixelValues": 16,
    "legend": {
        "dopplerApproaching": 18,
        "dopplerReceding": 19,
        "historyStart": 20,
        "lowReturn": 1,
        "mediumReturn": 8,
        "strongReturn": 13,
        "targetBorder": 17,
        "pixelColors": 16
    },
    "hasDoppler": true,
    "hasDualRange": true,
    "hasDualRadar": false,
    "noTransmitSectors": 2,
    "controls": {}
}))]
struct Capabilities {
    /// Maximum supported range in meters
    #[schema(example = 74080)]
    max_range: u32,
    /// Minimum supported range in meters
    #[schema(example = 50)]
    min_range: u32,
    /// List of all supported range values in meters
    #[schema(example = json!([50, 75, 100, 250, 500, 750, 1000, 1500, 2000, 3000]))]
    supported_ranges: Vec<u32>,
    /// Number of spokes (radial lines) per full rotation
    #[schema(example = 2048)]
    spokes_per_revolution: u16,
    /// Maximum number of samples per spoke
    #[schema(example = 1024)]
    max_spoke_length: u16,
    /// Number of distinct pixel intensity values
    #[schema(example = 16)]
    pixel_values: u8,
    /// Color mapping legend for interpreting spoke data (pixel value to color/type mapping)
    legend: Legend,
    /// Whether this radar supports Doppler velocity detection
    #[schema(example = true)]
    has_doppler: bool,
    /// Whether this radar supports simultaneous dual-range operation
    #[schema(example = true)]
    has_dual_range: bool,
    /// Whether this is part of a dual-radar system
    #[schema(example = false)]
    has_dual_radar: bool,
    /// Number of configurable no-transmit sectors
    #[schema(example = 2)]
    no_transmit_sectors: u8,
    /// Map of control IDs to their definitions and current state
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
                        ControlId::NoTransmitSector1
                            | ControlId::NoTransmitSector2
                            | ControlId::NoTransmitSector3
                            | ControlId::NoTransmitSector4
                    )
                })
                .count() as u8,
            controls,
        }
    }
}

#[utoipa::path(
    get,
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/capabilities",
    summary = "Get radar capabilities",
    description = "Returns static information about a specific radar including supported ranges, \
                   spoke resolution, Doppler support, and available controls. This information \
                   does not change during radar operation.",
    params(
        ("radar_id" = String, Path, description = "Radar identifier (e.g., 'nav1034A')", example = "nav1034A")
    ),
    responses(
        (status = 200, body = Capabilities, description = "Radar capabilities and control definitions"),
        (status = 404, description = "Radar not found")
    ),
    tag = "Radars"
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
    /// Radar identifier (e.g., 'nav1034A')
    #[schema(example = "nav1034A")]
    radar_id: String,
    /// Control identifier (e.g., 'gain', 'range', 'sea')
    #[schema(example = "gain")]
    control_id: String,
}

#[utoipa::path(
    put,
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}",
    summary = "Set a control value",
    description = "Sets the value of a specific radar control. The request body varies by control type: \
                   simple controls use 'value', controls with auto mode use 'value' and 'auto', \
                   guard zones use 'value', 'endValue', 'startDistance', 'endDistance', and 'enabled'.",
    params(
        ("radar_id" = String, Path, description = "Radar identifier", example = "nav1034A"),
        ("control_id" = String, Path, description = "Control identifier (e.g., gain, range, sea, guardZone1, ...)", example = "gain")
    ),
    request_body(
        content = BareControlValue,
        description = "Control value to set",
        example = json!({"value": 50, "auto": false})
    ),
    responses(
        (status = 200, description = "Control value set successfully"),
        (status = 400, description = "Invalid control name or value out of range"),
        (status = 404, description = "Radar not found")
    ),
    tag = "Controls"
)]
async fn set_control_value(
    Path(params): Path<RadarControlIdParam>,
    State(state): State<Web>,
    extract::Json(request): extract::Json<BareControlValue>,
) -> Response {
    let (radar_id, control_id) = (params.radar_id, params.control_id);
    log::info!(
        "PUT control {} = {:?} for radar {}",
        control_id,
        request,
        radar_id
    );

    // Get the radar info and control without holding the lock across await
    let (controls, control_value, radar_key) = {
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
                (radar.controls.clone(), control_value, radar.key())
            }
            None => {
                return RadarError::NoSuchRadar(radar_id).into_response();
            }
        }
    };
    // Lock is released here

    // Create a channel for the reply
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(1);

    // Check if this control should trigger persistence save
    let needs_persistence = matches!(
        control_value.id,
        ControlId::GuardZone1 | ControlId::GuardZone2 | ControlId::UserName
    );

    // Send the control request
    if let Err(e) = controls.process_client_request(control_value, reply_tx) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }

    // Save persistence for controls that need it
    if needs_persistence {
        state.radars.save_persistence(&radar_key);
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

#[utoipa::path(
    get,
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls/{control_id}",
    summary = "Get a control value",
    description = "Returns the current value and state of a specific radar control.",
    params(
        ("radar_id" = String, Path, description = "Radar identifier", example = "nav1034A"),
        ("control_id" = String, Path, description = "Control identifier", example = "Gain")
    ),
    responses(
        (status = 200, body = BareControlValue, description = "Current control value and state"),
        (status = 400, description = "Unknown control name"),
        (status = 404, description = "Radar not found")
    ),
    tag = "Controls"
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

/// Signal K formatted response wrapper
#[derive(Serialize, ToSchema)]
#[schema(example = json!({
    "version": "3.0.0",
    "radars": {
        "nav1034A": {
            "controls": {
                "gain": {"value": 50, "auto": false},
                "sea": {"value": 30, "auto": true, "autoValue": 25, "allowed": true},
                "range": {"value": 3000}
            }
        }
    }
}))]
struct FullSignalKResponse {
    /// API version
    #[schema(example = "3.0.0")]
    version: &'static str,
    /// Radar data nested by radar ID
    radars: Value,
}

#[utoipa::path(
    get,
    path = "/signalk/v2/api/vessels/self/radars/{radar_id}/controls",
    summary = "Get all control values",
    description = "Returns the current values of all radar controls for a specific radar. \
                   Controls include settings like Gain, Sea, Rain, Range, and operational modes.",
    params(
        ("radar_id" = String, Path, description = "Radar identifier", example = "nav1034A")
    ),
    responses(
        (status = 200, body = FullSignalKResponse, description = "All control values keyed by control name"),
        (status = 404, description = "Radar not found")
    ),
    tag = "Controls"
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
// =============================================================================
// WebSocket Stream Handler
// =============================================================================

/// Query parameters for WebSocket stream connection
#[derive(Deserialize, Debug, ToSchema)]
#[serde(rename_all = "camelCase")]
struct SignalKWebSocket {
    /// Initial subscription mode: 'all' (default), 'self', or 'none'
    #[schema(example = "all")]
    subscribe: Option<String>,
    /// Send cached control values on connect: 'true' (default) or 'false'
    #[schema(example = "true")]
    send_cached_values: Option<String>,
}

/// Documentation endpoint for the WebSocket stream (not actually called)
#[utoipa::path(
    get,
    path = "/signalk/v1/stream",
    summary = "Real-time control stream (WebSocket)",
    description = "WebSocket endpoint for real-time bidirectional radar control communication.\n\n\
## Connection\n\
Connect via WebSocket to receive real-time control value updates.\n\n\
## Query Parameters\n\
- `subscribe`: Initial subscription mode\n\
  - `all` (default): Subscribe to all control updates\n\
  - `self`: Subscribe to updates for the current vessel\n\
  - `none`: No initial subscriptions\n\
- `sendCachedValues`: Send current values on connect\n\
  - `true` (default): Send all current control values immediately\n\
  - `false`: Only send future updates\n\n\
## Client → Server Messages\n\n\
### Set Control Value\n\
Send a control command to change a radar setting:\n\
```json\n\
{\n\
  \"path\": \"radars.nav1034A.controls.gain\",\n\
  \"value\": 50\n\
}\n\
```\n\n\
For guard zones, include additional fields:\n\
```json\n\
{\n\
  \"path\": \"radars.nav1034A.controls.guardZone1\",\n\
  \"value\": 0,\n\
  \"endValue\": 90,\n\
  \"startDistance\": 100,\n\
  \"endDistance\": 500,\n\
  \"enabled\": true\n\
}\n\
```\n\n\
### Subscribe to Updates\n\
Subscribe to specific control paths with optional rate limiting:\n\
```json\n\
{\n\
  \"subscribe\": [\n\
    {\"path\": \"radars.*.controls.*\", \"period\": 1000},\n\
    {\"path\": \"radars.nav1034A.controls.gain\", \"policy\": \"instant\"}\n\
  ]\n\
}\n\
```\n\n\
Path patterns support wildcards:\n\
- `radars.*.controls.*` - all controls on all radars\n\
- `radars.nav1034A.controls.*` - all controls on specific radar\n\
- `*.gain` - gain control on all radars\n\n\
Subscription options:\n\
- `period`: Update interval in milliseconds (for fixed policy)\n\
- `minPeriod`: Minimum interval between updates\n\
- `policy`: Delivery policy\n\
  - `instant`: Send immediately when value changes\n\
  - `ideal`: Rate-limit to minPeriod\n\
  - `fixed`: Send at fixed intervals\n\n\
### Unsubscribe\n\
```json\n\
{\n\
  \"desubscribe\": [{\"path\": \"radars.*.controls.gain\"}]\n\
}\n\
```\n\n\
## Server → Client Messages\n\n\
### Delta Updates\n\
Control value changes are sent as delta messages:\n\
```json\n\
{\n\
  \"updates\": [{\n\
    \"$source\": \"mayara\",\n\
    \"timestamp\": \"2024-01-15T10:30:00Z\",\n\
    \"values\": [\n\
      {\"path\": \"radars.nav1034A.controls.gain\", \"value\": 50},\n\
      {\"path\": \"radars.nav1034A.controls.sea\", \"value\": 30, \"auto\": true}\n\
    ]\n\
  }]\n\
}\n\
```\n\n\
### Metadata\n\
On first connection, metadata describing each control is sent:\n\
```json\n\
{\n\
  \"updates\": [{\n\
    \"$source\": \"mayara\",\n\
    \"meta\": [\n\
      {\"path\": \"radars.nav1034A.controls.gain\", \"value\": {\"controlId\": \"gain\", \"type\": \"numeric\", ...}}\n\
    ]\n\
  }]\n\
}\n\
```",
    params(
        ("subscribe" = Option<String>, Query, description = "Initial subscription mode: 'all', 'self', or 'none'"),
        ("sendCachedValues" = Option<String>, Query, description = "Send cached values on connect: 'true' or 'false'")
    ),
    responses(
        (status = 101, description = "Switching Protocols - WebSocket connection established")
    ),
    tag = "Stream"
)]
#[allow(dead_code)]
async fn control_stream_docs() {}

async fn control_stream_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<SignalKWebSocket>,
    ws: WebSocketUpgrade,
) -> Response {
    log::debug!(
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

    log::debug!(
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
            let result = radar
                .controls
                .process_client_request(control_value.clone(), reply_tx);

            // Save persistence for controls that need it
            if result.is_ok()
                && matches!(
                    control_value.id,
                    ControlId::GuardZone1 | ControlId::GuardZone2 | ControlId::UserName
                )
            {
                radars.save_persistence(&radar.key());
            }

            result
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
    log::debug!("Sending {} subscribed controls", rcvs.len());
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
