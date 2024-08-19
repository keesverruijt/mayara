extern crate tokio;

use clap::Parser;
use env_logger::Env;
use log::{debug, info};
use tokio::task::JoinHandle;

// mod garmin;
mod locator;
mod navico;
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
}

#[tokio::main]
async fn main() -> Result<(), ()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .init();

    info!("Mayara {} loglevel {}", VERSION, log_level);

    let shutdown = tokio_shutdown::Shutdown::new().expect("shutdown creation works on first call");
    let shutdown_clone1 = shutdown.clone();
    let mut join_handles: Vec<JoinHandle<()>> = Vec::new();

    join_handles.push(tokio::spawn(async move {
        locator::new(shutdown.clone()).await.unwrap();
    }));
    join_handles.push(tokio::spawn(async move {
        web::new(args.port, shutdown_clone1).await.unwrap();
    }));

    for join_handle in join_handles {
        join_handle.await.unwrap();
    }
    debug!("waited for threads done");

    Ok(())
}
