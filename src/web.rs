use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use axum::{routing::get, Json, Router};
use log::info;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_shutdown::Shutdown;

use crate::VERSION;

pub async fn new(port: u16, shutdown: Shutdown) -> io::Result<()> {
    // our router
    let app = Router::new()
        .route("/", get(root))
        .route("/v1/api/radars", get(get_radars));
    //.route("/foo/bar", get(foo_bar));

    // run our app with hyper, listening globally on port 3000
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), port))
        .await
        .unwrap();
    info!(
        "HTTP server available on {:?}",
        listener.local_addr().unwrap()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.handle())
        .await
        .unwrap();
    Ok(())
}

async fn root() -> String {
    "Mayara".to_string() + VERSION
}

//
// Signal K radar API says this returns something like:
//    {"radar-0":{"id":"radar-0","name":"Navico","spokes":2048,"maxSpokeLen":1024,"streamUrl":"http://localhost:3001/v1/api/stream/radar-0"}}
//
async fn get_radars() -> Json<Value> {
    Json(json!({ "radar-0": 42 }))
}
