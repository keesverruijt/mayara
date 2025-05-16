extern crate tokio;

use clap::Parser;
//use env_logger::Env;
use locator::Locator;
//use log::{info, warn};
use miette::Result;
//use once_cell::sync::Lazy;
use radar::SharedRadars as SharedRadars;
use serde::{Serialize, Serializer};
use std::{
    collections::{HashMap, HashSet},
    net::{IpAddr, Ipv4Addr},
//    time::Duration,
};
use tokio::sync::{broadcast, mpsc};
use tokio_graceful_shutdown::{SubsystemBuilder, SubsystemHandle};
use once_cell::sync::OnceCell;

pub mod brand;
pub mod config;
pub mod locator;
pub mod navdata;
pub mod network;
pub mod protos;
pub mod radar;
pub mod settings;
pub mod util;
use rust_embed::RustEmbed;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard, PoisonError};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(RustEmbed, Clone)]
#[folder = "$OUT_DIR/web/"]
pub struct ProtoAssets;

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
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Brand {
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
pub struct InterfaceApi {
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


pub struct MayaraInner {
    pub args: Cli,
    pub tx_interface_request: broadcast::Sender<Option<mpsc::Sender<InterfaceApi>>>,
    pub radars: Option<SharedRadars>
}

pub struct Mayara {
    pub inner: Arc<RwLock<MayaraInner>>
}

impl Mayara {
    pub fn read(&self) -> Result<RwLockReadGuard<'_, MayaraInner>, PoisonError<RwLockReadGuard<'_, MayaraInner>>> {
        self.inner.read()
    }

    pub fn write(&self) -> Result<RwLockWriteGuard<'_, MayaraInner>, PoisonError<RwLockWriteGuard<'_, MayaraInner>>> {
        self.inner.write()
    }

    pub async fn new(
        subsystem: &SubsystemHandle,
        args: Cli
    ) -> Self {
        let (tx_interface_request, _) = broadcast::channel(10);
        let selfref = Mayara { inner: Arc::new(RwLock::new(MayaraInner {args, tx_interface_request, radars: None})) };
        let _ = GLOBAL_ARGS.set(selfref.clone());

        let mut navdata = navdata::NavigationData::new();

        let radars = Some(SharedRadars::new());

        selfref.write().unwrap().radars = radars;

        let locator = Locator::new(selfref.write().unwrap().radars.clone().unwrap());

        let (tx_ip_change, rx_ip_change) = mpsc::channel(1);

        subsystem.start(SubsystemBuilder::new("NavData", |a| async move {
            navdata.run(a, rx_ip_change).await
        }));
        let tx_interface_request = selfref.write().unwrap().tx_interface_request.clone();

        subsystem.start(SubsystemBuilder::new("Locator", |a| {
            locator.run(a, tx_ip_change, tx_interface_request)
        }));

        selfref
    }

    pub fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

static GLOBAL_ARGS: OnceCell<Mayara> = OnceCell::new();

pub fn get_global_args() -> Cli {
    GLOBAL_ARGS.get().expect("get_global_args() not yet initialized").read().unwrap().args.clone()
}

#[cfg(test)]
mod init {
    use ctor::ctor;
    use crate::{Cli, set_global_args};
    use clap::Parser;

    #[ctor]
    fn setup() {
        let _ = set_global_args(Cli::parse_from(["my_program"]));
    }
}