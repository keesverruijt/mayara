use axum::{
    Json, Router, debug_handler,
    extract::{ConnectInfo, Path, State},
    response::{IntoResponse, Response},
    routing::get,
};
use axum_embed::ServeEmbed;
use axum_openapi3::utoipa::openapi::{InfoBuilder, OpenApiBuilder};
// use axum_openapi3::utoipa::*; // Needed for ToSchema and IntoParams derive
use axum_openapi3::{
    build_openapi, // function for building the openapi spec
    endpoint,      // macro for defining endpoints
    reset_openapi, // function for cleaning the openapi cache (mostly used for testing)
};
use log::{debug, trace};
use miette::Result;
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use thiserror::Error;
use tokio::{net::TcpListener, sync::broadcast};
use tokio_graceful_shutdown::SubsystemHandle;

mod axum_fix;
mod v1;
mod v3;

use axum_fix::{Message, WebSocket, WebSocketUpgrade};
use mayara::{
    Session,
    radar::{RadarError, RadarInfo},
    settings::{ApiVersion, set_api_version},
};

#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/bin/web/"]
pub struct ProtoAssets;

const RADAR_URI: &str = "/v1/api/radars";
const INTERFACE_URI: &str = "/v1/api/interfaces";
const SPOKES_URI: &str = "/v1/api/spokes/";
const CONTROL_URI: &str = "/v1/api/control/";

//
// New v3 API endpoints are dispersed in the code and can be found under
// http://{host}:{port}/v3/openapi.json
//

#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Assets;

#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/bin/web/"]
struct ProtoWebAssets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct Web {
    session: Session,
    shutdown_tx: broadcast::Sender<()>,
}

impl Web {
    pub fn new(session: Session) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        Web {
            session,
            shutdown_tx,
        }
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        reset_openapi(); // clean the openapi cache. Mostly used for testing

        let port = self.session.read().unwrap().args.port.clone();
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port))
                .await
                .map_err(|e| WebError::Io(e))?;

        let serve_assets = ServeEmbed::<Assets>::new();
        let proto_web_assets = ServeEmbed::<ProtoWebAssets>::new();
        let proto_assets = ServeEmbed::<ProtoAssets>::new();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let router = Router::new()
            .route(&format!("{}{}", SPOKES_URI, "{key}"), get(spokes_handler))
            .route(&format!("{}{}", CONTROL_URI, "{key}"), get(control_handler));
        let router = v1::routes(router);
        let router = v3::routes(router);

        let app = router
            .nest_service("/protobuf", proto_web_assets)
            .nest_service("/proto", proto_assets)
            .fallback_service(serve_assets)
            .with_state(self)
            .into_make_service_with_connect_info::<SocketAddr>();

        log::info!("Starting HTTP web server on port {}", port);

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

    let ws = ws.accept_compression(true);

    match state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_by_id(&params.key)
        .clone()
    {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.key.to_string()).into_response(),
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

    let ws = ws.accept_compression(true);

    match state
        .session
        .read()
        .unwrap()
        .radars
        .as_ref()
        .unwrap()
        .get_by_id(&params.key)
        .clone()
    {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();

            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| control_stream(socket, radar, shutdown_rx))
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
                        set_api_version(ApiVersion::V1);
                        let message: String = serde_json::to_string(&message).unwrap();
                        set_api_version(ApiVersion::V3);
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
                        set_api_version(ApiVersion::V1);
                        let message: String = serde_json::to_string(&message).unwrap();
                        set_api_version(ApiVersion::V3);
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
