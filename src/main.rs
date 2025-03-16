extern crate tokio;

use clap::Parser;
use env_logger::Env;
use locator::Locator;
use log::{info, warn};
use miette::Result;
use once_cell::sync::Lazy;
use radar::SharedRadars;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

mod brand;
mod config;
mod locator;
mod protos;
mod radar;
mod settings;
mod signalk;
mod util;
mod web;

mod network;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Clone, Debug)]
pub struct Cli {
    #[clap(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    /// Port for webserver
    #[arg(short, long, default_value_t = 6502)]
    port: u16,

    /// Limit radar location to a single interface
    #[arg(short, long)]
    interface: Option<String>,

    /// Limit radar location to a single brand
    #[arg(short, long)]
    brand: Option<String>,

    /// Limit Signal K location to a single interface
    #[arg(short, long)]
    signalk_interface: Option<String>,

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    output: bool,

    /// Replay mode, see below
    #[arg(long, default_value_t = false)]
    replay: bool,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    fake_errors: bool,

    /// Allow wifi mode
    #[arg(long, default_value_t = false)]
    allow_wifi: bool,
}

static GLOBAL_ARGS: Lazy<Cli> = Lazy::new(|| Cli::parse());

#[tokio::main]
async fn main() -> Result<()> {
    let log_level = GLOBAL_ARGS.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .filter_module("tungstenite::protocol", log::LevelFilter::Info)
        .filter_module("mdns_sd", log::LevelFilter::Info)
        .filter_module("polling", log::LevelFilter::Info)
        .init();

    network::set_replay(GLOBAL_ARGS.replay);

    info!("Mayara {} loglevel {}", VERSION, log_level);
    if GLOBAL_ARGS.replay {
        warn!("Replay mode activated, this does the following:");
        warn!(" * A circle is drawn at the last two pixels in each spoke");
        warn!(" * Timestamp on each spoke is as if received now");
        warn!(" * Any 4G/HALO secondary radar B is ignored and not reported");
    }
    if GLOBAL_ARGS.fake_errors {
        warn!("Fake error mode activated, this does the following:");
        warn!(" * Any control operation on Rain Clutter beyond values 0..10 will fail");
        warn!(" * Failure for value 11..13 are all different");
    }
    if GLOBAL_ARGS.allow_wifi {
        warn!("Allow WiFi mode activated, this does the following:");
        warn!(" * Radars will be detected even on WiFi interfaces");
    }
    if GLOBAL_ARGS.output {
        warn!("Output mode activated; 'protobuf' formatted RadarMessage sent to stdout");
    }

    Toplevel::new(|s| async move {
        let signal_k = signalk::NavigationData::new();

        let radars = SharedRadars::new();
        let radars_clone1 = radars.clone();

        let locator = Locator::new(radars);
        let web = Web::new(GLOBAL_ARGS.port, radars_clone1);

        let (tx_ip_change, rx_ip_change) = mpsc::channel(1);

        s.start(SubsystemBuilder::new("SignalK", |a| async move {
            signal_k.run(a, rx_ip_change).await
        }));
        s.start(SubsystemBuilder::new("Locator", |a| {
            locator.run(a, tx_ip_change)
        }));
        s.start(SubsystemBuilder::new("Webserver", |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
