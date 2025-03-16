use axum::{
    debug_handler,
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, Path, State, WebSocketUpgrade,
    },
    http::Uri,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use axum_embed::ServeEmbed;
use hyper;
use log::{debug, trace};
use miette::Result;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str::FromStr,
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::broadcast, sync::mpsc};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::{
    radar::{Legend, RadarInfo, SharedRadars},
    settings::SharedControls,
    InterfaceApi, GLOBAL_ARGS,
};

const RADAR_URI: &str = "/v1/api/radars";
const INTERFACE_URI: &str = "/v1/api/interfaces";
const SPOKES_URI: &str = "/v1/api/spokes/";
const CONTROL_URI: &str = "/v1/api/control/";

#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Assets;

#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/web/"]
struct ProtoAssets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct Web {
    radars: SharedRadars,
    shutdown_tx: broadcast::Sender<()>,
    tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
}

impl Web {
    pub fn new(
        radars: SharedRadars,
        tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        Web {
            radars,
            shutdown_tx,
            tx_interface_request,
        }
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let listener = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            GLOBAL_ARGS.port,
        ))
        .await
        .map_err(|e| WebError::Io(e))?;

        let serve_assets = ServeEmbed::<Assets>::new();
        let proto_assets = ServeEmbed::<ProtoAssets>::new();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let app = Router::new()
            .route(RADAR_URI, get(get_radars))
            .route(INTERFACE_URI, get(get_interfaces))
            .route(&format!("{}{}", SPOKES_URI, "{key}"), get(spokes_handler))
            .route(&format!("{}{}", CONTROL_URI, "{key}"), get(control_handler))
            .nest_service("/proto", proto_assets)
            .fallback_service(serve_assets)
            .with_state(self)
            .into_make_service_with_connect_info::<SocketAddr>();

        tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                let _ = shutdown_tx.send(());
            },
            r = axum::serve(listener, app)
                    .with_graceful_shutdown(
                        async move {
                            _ = shutdown_rx.recv().await;
                        }
                    ) => {
                return r.map_err(|e| WebError::Io(e));
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApi {
    id: String,
    name: String,
    spokes: u16,
    max_spoke_len: u16,
    stream_url: String,
    control_url: String,
    legend: Legend,
    controls: SharedControls,
}

impl RadarApi {
    fn new(
        id: String,
        name: String,
        spokes: u16,
        max_spoke_len: u16,
        stream_url: String,
        control_url: String,
        legend: Legend,
        controls: SharedControls,
    ) -> Self {
        RadarApi {
            id: id,
            name: name,
            spokes,
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
//    {"radar-0":{"id":"radar-0","name":"Navico","spokes":2048,"maxSpokeLen":1024,"streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
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
        GLOBAL_ARGS.port
    );

    debug!("target host = '{}'", host);

    let mut api: HashMap<String, RadarApi> = HashMap::new();
    for info in state.radars.get_active() {
        let legend = &info.legend;
        let id = format!("radar-{}", info.id);
        let stream_url = format!("ws://{}{}{}", host, SPOKES_URI, id);
        let control_url = format!("ws://{}{}{}", host, CONTROL_URI, id);
        let name = info.controls.user_name();
        let v = RadarApi::new(
            id.to_owned(),
            name,
            info.spokes,
            info.max_spoke_len,
            stream_url,
            control_url,
            legend.clone(),
            info.controls.clone(),
        );

        api.insert(id.to_owned(), v);
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
    state.tx_interface_request.send(Some(tx)).unwrap();
    match rx.recv().await {
        Some(api) => Json(api).into_response(),
        _ => Json(Vec::<String>::new()).into_response(),
    }
}

#[derive(Deserialize)]
struct WebSocketHandlerParameters {
    key: String,
}

#[debug_handler]
async fn spokes_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<WebSocketHandlerParameters>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("stream request from {} for {}", addr, params.key);

    match state.radars.find_radar_info(&params.key) {
        Ok(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        Err(e) => e.into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)

async fn spokes_stream(
    mut socket: WebSocket,
    mut radar_message_rx: tokio::sync::broadcast::Receiver<Vec<u8>>,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Shutdown of websocket");
                break;
            },
            r = radar_message_rx.recv() => {
                match r {
                    Ok(message) => {
                        let len = message.len();
                        let ws_message = Message::Binary(message.into());
                        if let Err(e) = socket.send(ws_message).await {
                            debug!("Error on send to websocket: {}", e);
                            break;
                        }
                        trace!("Sent radar message {} bytes", len);
                    },
                    Err(e) => {
                        debug!("Error on RadarMessage channel: {}", e);
                        break;
                    }
                }
            }
        }
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

    match state.radars.find_radar_info(&params.key) {
        Ok(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();

            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| control_stream(socket, radar, shutdown_rx))
        }
        Err(e) => e.into_response(),
    }
}

/// Actual websocket statemachine (one will be spawned per connection)

async fn control_stream(
    mut socket: WebSocket,
    radar: RadarInfo,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let mut broadcast_control_rx = radar.all_clients_rx();
    let (reply_tx, mut reply_rx) = tokio::sync::mpsc::channel(60);

    if radar
        .controls
        .send_all_controls(reply_tx.clone())
        .await
        .is_err()
    {
        return;
    }

    debug!("Started /control websocket");

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                debug!("Shutdown of /control websocket");
                break;
            },
            // this is where we receive directed control messages meant just for us, they
            // are either error replies for an invalid control value or the full list of
            // controls.
            r = reply_rx.recv() => {
                match r {
                    Some(message) => {
                        let message = serde_json::to_string(&message).unwrap();
                        log::trace!("Sending {:?}", message);
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
                        let message: String = serde_json::to_string(&message).unwrap();
                        log::debug!("Sending {:?}", message);
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
