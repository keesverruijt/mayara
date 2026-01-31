use axum::{
    Json, Router, debug_handler,
    extract::{ConnectInfo, Path, State},
    response::{IntoResponse, Response},
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
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tokio_graceful_shutdown::SubsystemHandle;

mod axum_fix;
mod v1;
mod v3;

use axum_fix::{Message, WebSocket, WebSocketUpgrade};
use mayara::{
    Cli, InterfaceApi,
    radar::{RadarError, RadarInfo, SharedRadars},
    settings::set_api_version,
    start_session,
};

// Embedded files from the $project/web directory
#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Assets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct Web {
    radars: SharedRadars,
    args: Cli,
    shutdown_tx: broadcast::Sender<()>,
    tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
}

impl Web {
    pub async fn new(subsys: &SubsystemHandle, args: Cli) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);

        let (radars, tx_interface_request) = start_session(subsys, args.clone()).await;

        Web {
            radars,
            args,
            shutdown_tx,
            tx_interface_request,
        }
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        reset_openapi(); // clean the openapi cache. Mostly used for testing

        let port = self.args.port.clone();
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port))
                .await
                .map_err(|e| WebError::Io(e))?;

        let serve_assets = ServeEmbed::<Assets>::new();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let router = Router::new();
        let router = v1::routes(router); //.route_service("/v1", generated_assets_v1);
        let router = v3::routes(router); //.route_service("/v3", generated_assets_v3);

        let app = router
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
        OpenApiBuilder::new().info(InfoBuilder::new().title("Mayara").version("0.1.0"))
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

    match state.radars.get_by_id(&params.key).clone() {
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
