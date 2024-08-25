extern crate tokio;

use clap::Parser;
use env_logger::Env;
use locator::Locator;
use log::info;
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
mod util;
mod web;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
struct Cli {
    #[clap(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
    #[arg(short, long, default_value_t = 6502)]
    port: u16,
    #[arg(short, long)]
    interface: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .init();

    info!("Mayara {} loglevel {}", VERSION, log_level);

    let radars = Radars::new();
    let radars_clone1 = radars.clone();

    let locator = Locator::new(radars, args.interface);
    let web = Web::new(args.port, radars_clone1);

    Toplevel::new(|s| async move {
        s.start(SubsystemBuilder::new("Locator", |a| locator.run(a)));
        s.start(SubsystemBuilder::new("Webserver", |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
