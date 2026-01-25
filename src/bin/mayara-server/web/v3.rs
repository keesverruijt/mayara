use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::Uri,
    response::{IntoResponse, Response},
};
use axum_openapi3::utoipa::openapi::{InfoBuilder, OpenApiBuilder};
// use axum_openapi3::utoipa::*; // Needed for ToSchema and IntoParams derive
use axum_openapi3::{
    AddRoute,      // `add` method for Router to add routes also to the openapi spec
    build_openapi, // function for building the openapi spec
    endpoint,      // function for cleaning the openapi cache (mostly used for testing)
};
use hyper;
use log::debug;
use mayara::{
    radar::Legend,
    radar::RadarInfo,
    settings::{Control, ControlType},
};
use serde::Serialize;
use std::{collections::HashMap, net::SocketAddr, str::FromStr};
use tokio::sync::mpsc;

use super::SPOKES_URI;
use super::Web;

pub(super) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.add(get_radars())
        .add(get_interfaces())
        .add(get_radar())
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

    debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.session.read().unwrap().args.port
    );

    debug!("target host = '{}'", host);

    let mut api: HashMap<String, RadarApiV3> = HashMap::new();
    for info in state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_active()
        .clone()
    {
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}{}{}", host, SPOKES_URI, id);
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

    debug!("Interface state request from {} for host '{}'", addr, host);

    let (tx, mut rx) = mpsc::channel(1);
    state
        .session
        .read()
        .unwrap()
        .tx_interface_request
        .send(Some(tx))
        .unwrap();
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

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"Navico","streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
#[endpoint(
    method = "GET",
    path = "/v3/api/radar/{key}/capabilities",
    description = "Get all static information about a specific radar"
)]
async fn get_radar(
    Path(key): Path<String>,
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: hyper::header::HeaderMap,
) -> Response {
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };

    debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.session.read().unwrap().args.port
    );

    debug!("target host = '{}'", host);

    if let Some(info) = state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_by_id(&key)
        .clone()
    {
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}{}{}", host, SPOKES_URI, id);
        let name = info.controls.user_name();

        if let Some(controls) = info.controls.get_controls() {
            let v = Capabilities::new(id.to_owned(), name, stream_url, info, controls);

            return Json(v).into_response();
        }
    }
    Json(()).into_response()
}
