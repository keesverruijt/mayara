use anyhow::anyhow;
use axum::{
    body::Body,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use log::info;
use serde::Serialize;
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
};
use tokio::net::TcpListener;
use tokio_shutdown::Shutdown;

use crate::radar::Radars;
use crate::VERSION;

#[derive(Clone)]
struct WebState {
    radars: Arc<RwLock<Radars>>,
    url: String,
}

pub async fn new(port: u16, radars: Arc<RwLock<Radars>>, shutdown: Shutdown) -> io::Result<()> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port))
        .await
        .unwrap();

    let state = WebState {
        radars,
        url: format!("http://{}/v1/api/", listener.local_addr().unwrap()),
    };

    info!("HTTP server available on {}", &state.url);

    let app = Router::new()
        .route("/", get(root))
        .route("/v1/api/radars", get(get_radars).with_state(state));

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.handle())
        .await
        .unwrap();

    Ok(())
}

async fn root() -> String {
    "Mayara v".to_string() + VERSION
}

#[derive(Serialize)]
struct RadarApi {
    id: String,
    name: String,
    spokes: u16,
    max_spoke_len: u16,
    stream_url: String,
}

impl RadarApi {
    fn new(id: String, name: &str, spokes: u16, max_spoke_len: u16, stream_url: String) -> Self {
        RadarApi {
            id: id,
            name: name.to_owned(),
            spokes,
            max_spoke_len,
            stream_url,
        }
    }
}

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"Navico","spokes":2048,"maxSpokeLen":1024,"streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
async fn get_radars(State(state): State<WebState>, _request: Body) -> Response {
    match state.radars.read() {
        Ok(radars) => {
            let x = &radars.info;
            let mut api: HashMap<String, RadarApi> = HashMap::new();
            for (key, value) in x.iter() {
                let id = format!("radar-{}", value.id);
                let url = format!("{}stream/{}", state.url, id);
                api.insert(
                    id.to_owned(),
                    RadarApi::new(id, key, value.spokes, value.max_spoke_len, url),
                );
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
