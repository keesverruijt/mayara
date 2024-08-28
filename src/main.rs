extern crate tokio;

use clap::Parser;
use env_logger::Env;
use locator::Locator;
use log::{info, warn};
use miette::Result;
use radar::Radars;
use std::time::Duration;
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

// mod garmin;
mod locator;
mod navico;
mod protos;
mod radar;
mod settings;
mod util;
mod web;

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

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    output: bool,

    /// Replay or dev mode; draw circle at extreme range
    #[arg(long, default_value_t = false)]
    replay: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .init();

    info!("Mayara {} loglevel {}", VERSION, log_level);
    if args.replay {
        warn!("Replay mode activated, this does the following:");
        warn!(" * A circle is drawn at the last two pixels in each spoke");
        warn!(" * Timestamp on each spoke is as if received now");
        warn!(" * Any 4G/HALO secondary radar B is ignored and not reported");
    }

    if args.output {
        warn!("Output mode activated; 'protobuf' formatted RadarMessage sent to stdout");
    }

    let radars = Radars::new(args.clone());
    let radars_clone1 = radars.clone();

    let web = Web::new(args.port, radars_clone1);
    let locator = Locator::new(radars, args);

    Toplevel::new(|s| async move {
        s.start(SubsystemBuilder::new("Locator", |a| locator.run(a)));
        s.start(SubsystemBuilder::new("Webserver", |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
