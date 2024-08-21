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
use miette::Result;
use serde::Serialize;
use serde_with::skip_serializing_none;
use std::{
    collections::HashMap,
    fmt, io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, RwLock},
};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::Radars;
use crate::VERSION;

#[derive(Error, Debug)]
pub enum WebError {
    #[error("Socket operation failed")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct Web {
    radars: Arc<RwLock<Radars>>,
    url: Option<String>,
    port: u16,
}

impl Web {
    pub fn new(port: u16, radars: Arc<RwLock<Radars>>) -> Self {
        Web {
            radars,
            port,
            url: None,
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), WebError> {
        let listener = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            self.port,
        ))
        .await
        .unwrap();

        let url = format!("http://{}/v1/api/", listener.local_addr().unwrap());
        info!("HTTP server available on {}", &url);
        self.url = Some(url);

        let app = Router::new()
            .route("/", get(root))
            .route("/v1/api/radars", get(get_radars).with_state(self));

        let (close_tx, close_rx) = tokio::sync::oneshot::channel();

        tokio::select! { biased;
            _ = subsys.on_shutdown_requested() => {
                let _ = close_tx.send(());
            },
            r = axum::serve(listener, app)
                    .with_graceful_shutdown(
                        async move {
                            _ = close_rx.await;
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
enum PixelType {
    Normal,
    History,
    TargetBorder,
    DopplerApproaching,
    DopplerReceding,
}

struct Colour {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "#{:02x}{:02x}{:02x}{:02x}",
            self.r, self.g, self.b, self.a
        )
    }
}

use serde::ser::Serializer;

impl Serialize for Colour {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

#[skip_serializing_none]
#[derive(Serialize)]
struct Lookup {
    key: u8,
    value: Option<u8>,
    r#type: PixelType,
    colour: Colour,
}

#[derive(Serialize)]
struct RadarApi {
    id: String,
    name: String,
    spokes: u16,
    max_spoke_len: u16,
    stream_url: String,
    legend: Vec<Lookup>,
}

impl RadarApi {
    fn new(id: String, name: &str, spokes: u16, max_spoke_len: u16, stream_url: String) -> Self {
        RadarApi {
            id: id,
            name: name.to_owned(),
            spokes,
            max_spoke_len,
            stream_url,
            legend: Vec::new(),
        }
    }

    fn set_legend(&mut self, legend: Vec<Lookup>) {
        self.legend = legend;
    }
}

fn abs(n: f32) -> f32 {
    if n >= 0. {
        n
    } else {
        -n
    }
}

fn fake_legend() -> Vec<Lookup> {
    let mut legend = Vec::new();

    for history in 0..32 {
        let colour = Colour {
            r: 255,
            g: 255,
            b: 255,
            a: history * 4,
        };
        legend.push(Lookup {
            key: history,
            value: Some(history),
            r#type: PixelType::History,
            colour,
        });
    }
    for v in 0..13 {
        legend.push(Lookup {
            key: 32 + v,
            value: Some(v),
            r#type: PixelType::Normal,
            colour: Colour {
                r: (v as f32 * (200. / 13.)) as u8,
                g: (abs(7. - v as f32) * (200. / 13.)) as u8,
                b: ((13 - v) as f32 * (200. / 13.)) as u8,
                a: 255,
            },
        });
    }
    legend.push(Lookup {
        key: 32 + 13,
        value: None,
        r#type: PixelType::TargetBorder,
        colour: Colour {
            r: 200,
            g: 200,
            b: 200,
            a: 255,
        },
    });
    legend.push(Lookup {
        key: 32 + 13 + 1,
        value: None,
        r#type: PixelType::DopplerApproaching,
        colour: Colour {
            r: 0,
            g: 200,
            b: 200,
            a: 255,
        },
    });
    legend.push(Lookup {
        key: 32 + 13 + 2,
        value: None,
        r#type: PixelType::DopplerReceding,
        colour: Colour {
            r: 0x90,
            g: 0xd0,
            b: 0xf0,
            a: 255,
        },
    });

    legend
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
                let mut v =
                    RadarApi::new(id.to_owned(), &name, value.spokes, value.max_spoke_len, url);
                v.set_legend(fake_legend());
                api.insert(id.to_owned(), v);
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
