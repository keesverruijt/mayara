use anyhow::anyhow;
use axum::{
    body::Body,
    debug_handler,
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, Path, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use log::{debug, info, trace};
use miette::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::{Legend, Radars};
use crate::VERSION;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct Web {
    radars: Arc<RwLock<Radars>>,
    url: Option<String>,
    port: u16,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

#[derive(Deserialize)]
struct WebSocketHandlerParameters {
    key: String,
}

impl Web {
    pub fn new(port: u16, radars: Arc<RwLock<Radars>>) -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);

        Web {
            radars,
            port,
            url: None,
            shutdown_tx,
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let listener = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            self.port,
        ))
        .await
        .unwrap();

        let url = format!(
            "http://localhost:{}/v1/api/",
            listener.local_addr().unwrap().port()
        );
        info!("HTTP server available on {}", &url);
        self.url = Some(url);
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let app = Router::new()
            .route("/", get(root))
            .route("/v1/api/radars", get(get_radars))
            .route("/v1/api/stream/:key", get(ws_handler))
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
        };
        Ok(())
    }
}

async fn root() -> String {
    "Mayara v".to_string() + VERSION
}

#[derive(Serialize)]
struct RadarApi {
    id: String,
    name: String,
    spokes: u16,
    #[serde(rename = "maxSpokeLen")]
    max_spoke_len: u16,
    #[serde(rename = "streamUrl")]
    stream_url: String,
    legend: Legend,
}

impl RadarApi {
    fn new(
        id: String,
        name: &str,
        spokes: u16,
        max_spoke_len: u16,
        stream_url: String,
        legend: Legend,
    ) -> Self {
        RadarApi {
            id: id,
            name: name.to_owned(),
            spokes,
            max_spoke_len,
            stream_url,
            legend,
        }
    }
}

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"Navico","spokes":2048,"maxSpokeLen":1024,"streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
async fn get_radars(State(state): State<Web>, _request: Body) -> Response {
    match state.radars.read() {
        Ok(radars) => {
            let x = &radars.info;
            let mut api: HashMap<String, RadarApi> = HashMap::new();
            for (_key, value) in x.iter() {
                if let Some(legend) = &value.legend {
                    let id = format!("radar-{}", value.id);
                    let url = format!("{}stream/{}", state.url.as_ref().unwrap(), id);
                    let mut name = value.brand.to_owned();
                    if value.model.is_some() {
                        name.push(' ');
                        name.push_str(value.model.as_ref().unwrap());
                    }
                    if value.which.is_some() {
                        name.push(' ');
                        name.push_str(value.which.as_ref().unwrap());
                    }
                    let v = RadarApi::new(
                        id.to_owned(),
                        &name,
                        value.spokes,
                        value.max_spoke_len,
                        url,
                        legend.clone(),
                    );

                    api.insert(id.to_owned(), v);
                }
            }
            Json(api).into_response()
        }
        Err(_) => AppError(anyhow!("Poisoned lock")).into_response(),
    }
}

// Make our own error that wraps `anyhow::Error`.
struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

/// The handler for the HTTP request (this gets called when the HTTP GET lands at the start
/// of websocket negotiation). After this completes, the actual switching from HTTP to
/// websocket protocol will occur.
/// This is the last point where we can extract TCP/IP metadata such as IP address of the client
/// as well as things from HTTP headers such as user-agent of the browser etc.
#[debug_handler]
async fn ws_handler(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(params): Path<WebSocketHandlerParameters>,
    ws: WebSocketUpgrade,
) -> Response {
    debug!("stream request from {} for {}", addr, params.key);

    match match_radar_id(&state, &params.key) {
        Ok(radar_message_rx) => {
            let shutdown_rx = state.shutdown_tx.subscribe();
            // finalize the upgrade process by returning upgrade callback.
            // we can customize the callback by sending additional info such as address.
            ws.on_upgrade(move |socket| handle_socket(socket, radar_message_rx, shutdown_rx))
        }
        Err(e) => e.into_response(),
    }
}

fn match_radar_id(
    state: &Web,
    key: &str,
) -> Result<tokio::sync::broadcast::Receiver<Vec<u8>>, AppError> {
    match state.radars.read() {
        Ok(radars) => {
            let x = &radars.info;

            for (_key, value) in x.iter() {
                if value.legend.is_some() {
                    let id = format!("radar-{}", value.id);
                    if id == key {
                        return Ok(value.radar_message_tx.subscribe());
                    }
                }
            }
        }
        Err(_) => return Err(AppError(anyhow!("Poisoned lock"))),
    }
    Err(AppError(anyhow!("No such radar {}", key)))
}
/// Actual websocket statemachine (one will be spawned per connection)

async fn handle_socket(
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
                        let ws_message = Message::Binary(message);
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
