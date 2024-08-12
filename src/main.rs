extern crate tokio;

use clap::Parser;
use env_logger::Env;
use log::info;

mod locator;

/// Search for a pattern in a file and display the lines that contain it.
#[derive(Parser)]
struct Cli {
    #[clap(flatten)]
    verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,
}

#[tokio::main]
async fn main() -> Result<(), ()> {
    let args = Cli::parse();

    let log_level = args.verbose.log_level_filter();
    env_logger::Builder::from_env(Env::default())
        .filter_level(log_level)
        .init();

    info!("Mayara v0.xx loglevel {}", log_level);

    let shutdown = tokio_shutdown::Shutdown::new().expect("shutdown creation works on first call");

    // Start the locators which will populate our known radar list
    let join_handle = tokio::spawn(async move {
        locator::new(shutdown.clone()).await.unwrap();
    });

    join_handle.await.unwrap();
    println!("waiting for threads done");

    Ok(())
}
