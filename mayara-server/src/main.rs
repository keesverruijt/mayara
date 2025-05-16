extern crate tokio;

use clap::Parser;
use env_logger::Env;
//use locator::Locator;
use log::{info, warn};
use miette::Result;
//use once_cell::sync::Lazy;
//use radar::SharedRadars;
//use serde::{Serialize, Serializer};
use std::{
    time::Duration,
};
//use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, Toplevel};
use web::Web;

/*
use brand;
mod config;
mod locator;
mod navdata;
mod network;
mod protos;
mod radar;
mod settings;
mod util;
*/
mod web;

use mayara;
use mayara::{network, Cli, Session};

/*

#[derive(clap::ValueEnum, Clone, Default, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum TargetMode {
    #[default]
    Arpa,
    Trails,
    None,
}

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
    brand: Option<Brand>,

    /// Target analysis mode
    #[arg(short, long, default_value_t, value_enum)]
    targets: TargetMode,

    /// Set navigation service address, either
    /// - Nothing: all interfaces will search via MDNS
    /// - An interface name: only that interface will seach for via MDNS
    /// - `udp-listen:ipv4-address:port` = listen on (broadcast) address at given port
    #[arg(short, long)]
    navigation_address: Option<String>,

    /// Use NMEA 0183 for navigation service instead of Signal K
    #[arg(long)]
    nmea0183: bool,

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    output: bool,

    /// Replay mode, see below
    #[arg(short, long, default_value_t = false)]
    replay: bool,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    fake_errors: bool,

    /// Allow wifi mode
    #[arg(long, default_value_t = false)]
    allow_wifi: bool,

    /// Stationary mode
    #[arg(long, default_value_t = false)]
    stationary: bool,
}

static get_global_args(): Lazy<Cli> = Lazy::new(|| Cli::parse());

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Brand {
    Furuno,
    Garmin,
    Navico,
    Raymarine,
}

impl Into<Brand> for &str {
    fn into(self) -> Brand {
        match self.to_ascii_lowercase().as_str() {
            "furuno" => Brand::Furuno,
            "garmin" => Brand::Garmin,
            "navico" => Brand::Navico,
            "raymarine" => Brand::Raymarine,
            _ => panic!("Invalid brand"),
        }
    }
}

impl Serialize for Brand {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Furuno => serializer.serialize_str("Furuno"),
            Self::Garmin => serializer.serialize_str("Garmin"),
            Self::Navico => serializer.serialize_str("Navico"),
            Self::Raymarine => serializer.serialize_str("Raymarine"),
        }
    }
}

impl std::fmt::Display for Brand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Furuno => write!(f, "Furuno"),
            Self::Garmin => write!(f, "Garmin"),
            Self::Navico => write!(f, "Navico"),
            Self::Raymarine => write!(f, "Raymarine"),
        }
    }
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct RadarInterfaceApi {
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip)]
    ip: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    listeners: Option<HashMap<Brand, String>>,
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct InterfaceId {
    name: String,
}
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct InterfaceApi {
    brands: HashSet<Brand>,
    interfaces: HashMap<InterfaceId, RadarInterfaceApi>,
}

impl RadarInterfaceApi {
    fn new(
        status: Option<String>,
        ip: Option<Ipv4Addr>,
        listeners: Option<HashMap<Brand, String>>,
    ) -> Self {
        Self {
            status,
            ip,
            listeners,
        }
    }
}

impl InterfaceId {
    fn new(name: &str, address: Option<&IpAddr>) -> Self {
        Self {
            name: match address {
                Some(addr) => format!("{} ({})", name, addr),
                None => name.to_owned(),
            },
        }
    }
}

impl Serialize for InterfaceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.name.as_str())
    }
}
*/

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

    network::set_replay(args.replay);

    info!("Mayara {} loglevel {}", mayara::VERSION, log_level);
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
    if args.allow_wifi {
        warn!("Allow WiFi mode activated, this does the following:");
        warn!(" * Radars will be detected even on WiFi interfaces");
    }
    if args.output {
        warn!("Output mode activated; 'protobuf' formatted RadarMessage sent to stdout");
    }
    if args.nmea0183 {
        warn!(
            "NMEA0183 mode activated; will load GPS position, heading and date/time from {}",
            args
                .navigation_address
                .as_ref()
                .unwrap_or(&"MDNS".to_string())
        );
    }

    Toplevel::new(|s| async move {
       let mr = Session::new(&s, args).await;
       let m = mr.read().unwrap();

       let web = Web::new(m.radars.as_ref().unwrap().clone(), m.tx_interface_request.clone());
       s.start(SubsystemBuilder::new("Webserver", move |a| web.run(a)));
    })
    .catch_signals()
    .handle_shutdown_requests(Duration::from_millis(5000))
    .await
    .map_err(Into::into)
}
