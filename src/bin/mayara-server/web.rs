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
use http::Uri;
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
use tokio::{
    net::TcpListener,
    sync::{broadcast, mpsc},
};
use tokio_graceful_shutdown::SubsystemHandle;
use utoipa::ToSchema;

mod axum_fix;
mod v1;
mod v3;

use axum_fix::{Message, WebSocket, WebSocketUpgrade};
use mayara::{
    Cli, InterfaceApi, PACKAGE, VERSION,
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

        let router = Router::new().route("/", get(endpoints));
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

// {
//   "endpoints": {
//     "v1": {
//       "version": "1.0.0-alpha1",
//       "signalk-http": "http://localhost:3000/signalk/v1/api/",
//       "signalk-ws": "ws://localhost:3000/signalk/v1/stream"
//     },
//     "v3": {
//       "version": "3.0.0",
//       "signalk-http": "http://localhost/signalk/v3/api/",
//       "signalk-ws": "ws://localhost/signalk/v3/stream",
//       "signalk-tcp": "tcp://localhost:8367"
//     }
//   },
//   "server": {
//     "id": "signalk-server-node",
//     "version": "0.1.33"
//   }
// }

#[derive(Serialize, ToSchema)]
struct Endpoints {
    endpoints: HashMap<String, Endpoint>,
    server: Server,
}

#[derive(Serialize, ToSchema)]
struct Endpoint {
    version: String,
    #[serde(rename = "signalk-http")]
    http: String,
    #[serde(rename = "signalk-ws")]
    ws: String,
}
#[derive(Serialize, ToSchema)]
struct Server {
    version: &'static str,
    id: &'static str,
}

async fn endpoints(State(state): State<Web>, headers: hyper::header::HeaderMap) -> Json<Endpoints> {
    log::debug!("endpoints: headers: {:?}", headers);
    let host: String = match headers.get(axum::http::header::HOST) {
        Some(host) => host.to_str().unwrap_or("localhost").to_string(),
        None => "localhost".to_string(),
    };
    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.args.port
    );

    let mut endpoints = Endpoints {
        endpoints: HashMap::new(),
        server: Server {
            version: VERSION,
            id: PACKAGE,
        },
    };
    endpoints.endpoints.insert(
        "v1".to_string(),
        Endpoint {
            version: "1.0.0".to_string(),
            http: format!("http://{}/v1/api/", host),
            ws: format!("ws://{}/v1/api/stream", host),
        },
    );
    endpoints.endpoints.insert(
        "v3".to_string(),
        Endpoint {
            version: "3.0.0".to_string(),
            http: format!("http://{}/v3/api/", host),
            ws: format!("ws://{}/v3/api/stream", host),
        },
    );

    Json(endpoints)
}

#[endpoint(
    method = "GET",
    path = "/v3/api/resource/openapi.json",
    description = "OpenAPI spec"
)]
async fn openapi(State(_state): State<Web>) -> impl IntoResponse {
    // `build_openapi` caches the openapi spec, so it's not necessary to call it every time
    let openapi = build_openapi(|| {
        OpenApiBuilder::new().info(InfoBuilder::new().title("mayara").version(VERSION))
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

    match state.radars.get_by_key(&params.key) {
        Some(radar) => {
            let shutdown_rx = state.shutdown_tx.subscribe();
            let radar_message_rx = radar.message_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| spokes_stream(socket, radar_message_rx, shutdown_rx))
        }
        None => RadarError::NoSuchRadar(params.key).into_response(),
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
