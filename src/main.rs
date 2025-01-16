extern crate tokio;

use clap::Parser;
use env_logger::Env;
use locator::Locator;
use log::{info, warn};
use miette::Result;
use radar::SharedRadars;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

mod config;
// mod garmin;
#[cfg(feature = "furuno")]
mod furuno;
mod locator;
#[cfg(feature = "navico")]
mod navico;
mod protos;
mod radar;
#[cfg(feature = "raymarine")]
mod raymarine;
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

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    output: bool,

    /// Replay mode, see below
    #[arg(long, default_value_t = false)]
    replay: bool,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    fake_errors: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .filter_module("tungstenite::protocol", log::LevelFilter::Info)
        .filter_module("mdns_sd", log::LevelFilter::Info)
        .filter_module("polling", log::LevelFilter::Info)
        .init();

    info!("Mayara {} loglevel {}", VERSION, log_level);
    if args.replay {
        warn!("Replay mode activated, this does the following:");
        warn!(" * A circle is drawn at the last two pixels in each spoke");
        warn!(" * Timestamp on each spoke is as if received now");
        warn!(" * Any 4G/HALO secondary radar B is ignored and not reported");
    }
    if args.fake_errors {
        warn!("Fake error mode activated, this does the following:");
        warn!(" * Any control operation on Rain Clutter beyond values 0..10 will fail");
        warn!(" * Failure for value 11..13 are all different");
    }
    if args.output {
        warn!("Output mode activated; 'protobuf' formatted RadarMessage sent to stdout");
    }

    let signal_k = signalk::NavigationData::new(args.clone());
    let radars = SharedRadars::new(args.clone());
    let radars_clone1 = radars.clone();

    let locator = Locator::new(radars);
    let web = Web::new(args.port, radars_clone1);

    Toplevel::new(|s| async move {
        s.start(SubsystemBuilder::new("SignalK", |a| signal_k.run(a)));
        s.start(SubsystemBuilder::new("Locator", |a| locator.run(a)));
        s.start(SubsystemBuilder::new("Webserver", |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
