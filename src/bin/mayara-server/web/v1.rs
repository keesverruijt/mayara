use axum::{
    Json, debug_handler,
    extract::{ConnectInfo, State},
    http::Uri,
    response::{IntoResponse, Response},
    routing::get,
};
use hyper;
use log::debug;
use num_traits::ToPrimitive;
use serde::Serialize;
use std::{collections::HashMap, net::SocketAddr, str::FromStr};
use tokio::sync::mpsc;

use super::{
    Message, Path, RadarInfo, Web, WebSocket, WebSocketHandlerParameters, WebSocketUpgrade,
    set_api_version, spokes_handler,
};

use mayara::{
    radar::{Legend, RadarError},
    settings::{ApiVersion, Control},
};

const RADAR_URI: &str = "/v1/api/radars";
const INTERFACE_URI: &str = "/v1/api/interfaces";
pub(super) const SPOKES_URI: &str = "/v1/api/spokes/";
const CONTROL_URI: &str = "/v1/api/control/";

pub(super) fn routes(axum: axum::Router<Web>) -> axum::Router<Web> {
    axum.route(&format!("{}{}", SPOKES_URI, "{key}"), get(spokes_handler))
        .route(RADAR_URI, get(get_radars))
        .route(INTERFACE_URI, get(get_interfaces))
        .route(&format!("{}{}", CONTROL_URI, "{key}"), get(control_handler))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApi {
    id: String,
    name: String,
    spokes_per_revolution: u16,
    max_spoke_len: u16,
    stream_url: String,
    control_url: String,
    legend: Legend,
    controls: HashMap<u8, Control>,
}

impl RadarApi {
    fn new(
        id: String,
        name: String,
        spokes_per_revolution: u16,
        max_spoke_len: u16,
        stream_url: String,
        control_url: String,
        legend: Legend,
        controls: HashMap<u8, Control>,
    ) -> Self {
        RadarApi {
            id: id,
            name: name,
            spokes_per_revolution,
            max_spoke_len,
            stream_url,
            control_url,
            legend,
            controls,
        }
    }
}

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"Navico","streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
#[debug_handler]
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

    let mut api: HashMap<String, RadarApi> = HashMap::new();
    for info in state.session.read().unwrap().radars.get_active().clone() {
        let legend = &info.legend;
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}{}{}", host, SPOKES_URI, id);
        let control_url = format!("ws://{}{}{}", host, CONTROL_URI, id);
        let name = info.controls.user_name();

        if let Some(controls) = info.controls.get_controls() {
            let mut control_list: HashMap<u8, Control> = HashMap::with_capacity(controls.len());
            for (ctype, control) in controls.iter() {
                let key = ctype.to_u8().unwrap();
                control_list.insert(key, control.clone());
            }

            let v = RadarApi::new(
                id.to_owned(),
                name,
                info.spokes_per_revolution,
                info.max_spoke_len,
                stream_url,
                control_url,
                legend.clone(),
                control_list,
            );

            api.insert(id.to_owned(), v);
        }
    }
    Json(api).into_response()
}

#[debug_handler]
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

#[debug_handler]
async fn control_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<WebSocketHandlerParameters>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("control request from {} for {}", addr, params.key);

    let ws = ws.accept_compression(true);

    match state
        .session
        .read()
        .unwrap()
        .radars
        .get_by_id(&params.key)
        .clone()
    {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();

            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| control_stream(socket, radar, ApiVersion::V1, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.key.to_string()).into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)
/// This websocket handler is only for the v1 API, as v2/v3 uses REST for controls
///
async fn control_stream(
    mut socket: WebSocket,
    radar: RadarInfo,
    api_version: ApiVersion,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut broadcast_control_rx = radar.all_clients_rx();
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(60);

    log::debug!("Starting /control v1 websocket");

    if radar
        .controls
        .send_all_controls(reply_tx.clone())
        .await
        .is_err()
    {
        return;
    }

    log::debug!("Started /control v1 websocket");

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                log::debug!("Shutdown of /control websocket");
                break;
            },
            // this is where we receive directed control messages meant just for us, they
            // are either error replies for an invalid control value or the full list of
            // controls.
            r = reply_rx.recv() => {
                match r {
                    Some(message) => {
                        // Note: temporarily set API version to V1 for serialization, no await in between
                        if api_version != ApiVersion::V3 {
                            set_api_version(api_version);
                        }
                        let message: String = serde_json::to_string(&message).unwrap();
                        if api_version != ApiVersion::V3 {
                            set_api_version(ApiVersion::V3);
                        }
                        let ws_message = Message::Text(message.into());

                        if let Err(e) = socket.send(ws_message).await {
                            log::error!("send to websocket client: {e}");
                            break;
                        }

                    },
                    None => {
                        log::error!("Error on Control channel");
                        break;
                    }
                }
            },
            r = broadcast_control_rx.recv() => {
                match r {
                    Ok(message) => {
                        // Note: temporarily set API version to V1 for serialization, no await in between
                        if api_version != ApiVersion::V3 {
                            set_api_version(api_version);
                        }
                        let message: String = serde_json::to_string(&message).unwrap();
                        if api_version != ApiVersion::V3 {
                            set_api_version(ApiVersion::V3);
                        }
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
                                if let Ok(control_value) = serde_json::from_str(&message) {
                                    log::debug!("Received ControlValue {:?}", control_value);
                                    let _ = radar.controls.process_client_request(control_value, reply_tx.clone()).await;
                                } else {
                                    log::error!("Unknown JSON string '{}'", message);
                                }

                            },
                            _ => {
                                debug!("Dropping unexpected message {:?}", message);
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
