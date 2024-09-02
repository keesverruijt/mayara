use anyhow::anyhow;
use axum::{
    extract::{ ConnectInfo, Host, State },
    http::{ StatusCode, Uri },
    response::{ IntoResponse, Response },
    routing::get,
    Json,
    Router,
};
use log::debug;
use miette::Result;
use serde::Serialize;
use std::{
    collections::HashMap,
    io,
    net::{ IpAddr, Ipv4Addr, SocketAddr },
    str::FromStr,
    sync::{ Arc, RwLock },
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_graceful_shutdown::SubsystemHandle;
use rust_embed::RustEmbed;
use axum_embed::ServeEmbed;

use crate::radar::{ Legend, Radars };
use crate::VERSION;

#[derive(RustEmbed, Clone)]
#[folder = "web/"]
struct Assets;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")] Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct Web {
    radars: Arc<RwLock<Radars>>,
    port: u16,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl Web {
    pub fn new(port: u16, radars: Arc<RwLock<Radars>>) -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);

        Web {
            radars,
            port,
            shutdown_tx,
        }
    }

    pub async fn run(self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let listener = TcpListener::bind(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), self.port)
        ).await.unwrap();

        let serve_assets = ServeEmbed::<Assets>::new();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let shutdown_tx = self.shutdown_tx.clone(); // Clone as self used in with_state() and with_graceful_shutdown() below

        let app = Router::new()
            .route("/v1/api/radars", get(get_radars))
            .nest_service("/", serve_assets)
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

async fn root() -> String {
    "Mayara v".to_string() + VERSION
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RadarApi {
    id: String,
    name: String,
    spokes: u16,
    max_spoke_len: u16,
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
        legend: Legend
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
async fn get_radars(
    State(state): State<Web>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Host(host): Host
) -> Response {
    debug!("Radar state request from {} for host '{}'", addr, host);

    let host = format!(
        "{}:{}",
        match Uri::from_str(&host) {
            Ok(uri) => uri.host().unwrap_or("localhost").to_string(),
            Err(_) => "localhost".to_string(),
        },
        state.port + 1
    );

    debug!("target host = '{}'", host);

    match state.radars.read() {
        Ok(radars) => {
            let x = &radars.info;
            let mut api: HashMap<String, RadarApi> = HashMap::new();
            for (_key, value) in x.iter() {
                if let Some(legend) = &value.legend {
                    let id = format!("radar-{}", value.id);
                    let url = format!("http://{}/v1/api/stream/{}", host, id);
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
                        legend.clone()
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
        ).into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError where E: Into<anyhow::Error> {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
