extern crate tokio;

use clap::Parser;
use locator::Locator;
use miette::Result;
use radar::SharedRadars;
use serde::{Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
};
use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use utoipa::ToSchema;

pub mod brand;
pub mod config;
pub mod locator;
pub mod navdata;
pub mod network;
pub mod protos;
pub mod radar;
pub mod stream;
pub mod util;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PACKAGE: &str = env!("CARGO_PKG_NAME");

#[derive(clap::ValueEnum, Clone, Default, Debug, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetMode {
    #[default]
    Arpa,
    Trails,
    None,
}

#[derive(Parser, Clone, Debug)]
pub struct Cli {
    #[clap(flatten)]
    pub verbose: clap_verbosity_flag::Verbosity<clap_verbosity_flag::InfoLevel>,

    /// Port for webserver
    #[arg(short, long, default_value_t = 6502)]
    pub port: u16,

    /// Limit radar location to a single interface
    #[arg(short, long)]
    pub interface: Option<String>,

    /// Limit radar location to a single brand
    #[arg(short, long)]
    pub brand: Option<Brand>,

    /// Target analysis mode
    #[arg(short, long, default_value_t, value_enum)]
    pub targets: TargetMode,

    /// Set navigation service address, either
    /// - Nothing: all interfaces will search via MDNS
    /// - An interface name: only that interface will seach for via MDNS
    /// - `udp-listen:ipv4-address:port` = listen on (broadcast) address at given port
    #[arg(short, long)]
    pub navigation_address: Option<String>,

    /// Use NMEA 0183 for navigation service instead of Signal K
    #[arg(long)]
    pub nmea0183: bool,

    /// Write RadarMessage data to stdout
    #[arg(long, default_value_t = false)]
    pub output: bool,

    /// Replay mode, see below
    #[arg(short, long, default_value_t = false)]
    pub replay: bool,

    /// Fake error mode, see below
    #[arg(long, default_value_t = false)]
    pub fake_errors: bool,

    /// Allow wifi mode
    #[arg(long, default_value_t = false)]
    pub allow_wifi: bool,

    /// Stationary mode
    #[arg(long, default_value_t = false)]
    pub stationary: bool,

    /// Multi-radar mode keeps locators running even when one radar is found
    #[arg(long, default_value_t = false)]
    pub multiple_radar: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash, ToSchema)]
pub enum Brand {
    Furuno,
    Garmin,
    Navico,
    Raymarine,
}

impl Brand {
    pub fn to_prefix(&self) -> &'static str {
        match self {
            Self::Furuno => "fur",
            Self::Garmin => "gar",
            Self::Navico => "nav",
            Self::Raymarine => "ray",
        }
    }
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

#[derive(Serialize, Clone, ToSchema)]
enum InterfaceStatus {
    Ok,
    NoIPv4Address,
    WirelessIgnored,
}

/// Information about a network interface and its radar listeners
#[derive(Serialize, Clone, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(example = json!({
    "ip": "192.168.1.100",
    "netmask": "255.255.255.0",
    "listeners": {
        "Furuno": "No match for 172.31.255.255",
        "Navico": "Active",
        "Raymarine": "Listening"
    }
}))]
struct RadarInterfaceApi {
    // Interface status: null (ok), "No IPv4 address" or "Wireless ignored"
    status: InterfaceStatus,
    /// IPv4 address assigned to this interface
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, example = "192.168.1.100")]
    ip: Option<Ipv4Addr>,
    /// Network mask for this interface
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, example = "255.255.255.0")]
    netmask: Option<Ipv4Addr>,
    /// Map of radar brand to listener status message
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<HashMap<String, String>>, example = json!({"Navico": "Active"}))]
    listeners: Option<HashMap<Brand, String>>,
}

/// Network interface identifier (e.g., "en0", "eth0")
#[derive(Clone, Eq, PartialEq, Hash, ToSchema)]
#[schema(as = String, example = "en0")]
struct InterfaceId {
    name: String,
}

/// API response containing network interface information for radar detection
#[derive(Serialize, Clone, ToSchema)]
#[schema(example = json!({
    "brands": ["Navico", "Furuno"],
    "interfaces": {
        "en0": {
            "status": "up",
            "ip": "192.168.1.100",
            "netmask": "255.255.255.0",
            "listeners": {
                "Navico": "Active",
                "Furuno": "No match for 172.31.255.255"
            }
        },
        "en1": {
            "status": "Wireless ignored"
        }
    }
}))]
pub struct InterfaceApi {
    /// Set of radar brands that have been compiled into this server
    #[schema(example = json!(["Navico", "Furuno"]))]
    brands: HashSet<Brand>,
    /// Map of network interface name to its radar listener information
    #[schema(value_type = HashMap<String, RadarInterfaceApi>)]
    interfaces: HashMap<InterfaceId, RadarInterfaceApi>,
}

impl RadarInterfaceApi {
    fn new(
        status: InterfaceStatus,
        ip: Option<Ipv4Addr>,
        netmask: Option<Ipv4Addr>,
        listeners: Option<HashMap<Brand, String>>,
    ) -> Self {
        Self {
            status,
            ip,
            netmask,
            listeners,
        }
    }
}

impl InterfaceId {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
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

pub async fn start_session(
    subsystem: &SubsystemHandle,
    args: Cli,
) -> (
    SharedRadars,
    broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
) {
    let radars = SharedRadars::new();
    let (tx_interface_request, _) = broadcast::channel(10);

    let locator = Locator::new(args.clone(), radars.clone());

    let (tx_ip_change, _rx_ip_change) = broadcast::channel(1);
    let mut navdata = navdata::NavigationData::new(args.clone());

    let rx_ip_change_clone = tx_ip_change.subscribe();
    subsystem.start(SubsystemBuilder::new("NavData", |subsys| async move {
        navdata.run(subsys, rx_ip_change_clone).await
    }));
    let tx_interface_request_clone = tx_interface_request.clone();
    subsystem.start(SubsystemBuilder::new("Locator", |subsys| {
        locator.run(subsys, tx_ip_change, tx_interface_request_clone)
    }));

    (radars, tx_interface_request)
}
