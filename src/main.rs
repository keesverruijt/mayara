extern crate tokio;

use clap::Parser;
use env_logger::Env;
use log::{debug, error, info};
use radar::Radars;
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
    #[arg(short, long)]
    interface: Option<String>,
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

    let radars = Radars::new();
    let radars_clone1 = radars.clone();

    join_handles.push(tokio::spawn(async move {
        if let Err(e) = locator::new(&radars, args.interface, shutdown).await {
            error!("Location of radar failed: {}", e);
        }
    }));
    join_handles.push(tokio::spawn(async move {
        web::new(args.port, radars_clone1, shutdown_clone1)
            .await
            .unwrap();
    }));

    for join_handle in join_handles {
        let _ = join_handle.await;
    }
    debug!("waited for threads done");

    Ok(())
}
