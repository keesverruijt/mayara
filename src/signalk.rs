use atomic_float::AtomicF64;
use futures_util::future::select_ok;
use mdns_sd::{Error, IfKind, ServiceDaemon, ServiceEvent};
use serde_json::Value;
use std::{
    collections::HashSet,
    future::Future,
    io::ErrorKind,
    net::SocketAddr,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncBufReadExt, BufWriter};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio::{io::BufReader, sync::mpsc::Receiver};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::{
    radar::{GeoPosition, RadarError},
    Cli,
};

/// The hostname of the devices we are searching for.
/// Every Chromecast will respond to the service name in this example.
const TCP_SERVICE_NAME: &'static str = "_signalk-tcp._tcp.local.";

const SUBSCRIBE: &'static str =
       "{\"context\": \"vessels.self\",\"subscribe\": [{\"path\": \"navigation.headingTrue\"},{\"path\": \"navigation.position\"}]}\r\n";

static HEADING_TRUE_VALID: AtomicBool = AtomicBool::new(false);
static POSITION_VALID: AtomicBool = AtomicBool::new(false);
static HEADING_TRUE: AtomicF64 = AtomicF64::new(0.0);
static POSITION_LAT: AtomicF64 = AtomicF64::new(0.0);
static POSITION_LON: AtomicF64 = AtomicF64::new(0.0);

pub(crate) fn get_heading_true() -> Option<f64> {
    if HEADING_TRUE_VALID.load(Ordering::Acquire) {
        return Some(HEADING_TRUE.load(Ordering::Acquire));
    }
    return None;
}

pub(crate) fn get_radar_position() -> Option<GeoPosition> {
    if POSITION_VALID.load(Ordering::Acquire) {
        let lat = POSITION_LAT.load(Ordering::Acquire);
        let lon = POSITION_LON.load(Ordering::Acquire);
        return Some(GeoPosition::new(lat, lon));
    }
    return None;
}

pub(crate) fn get_position_i64() -> (Option<i64>, Option<i64>) {
    if POSITION_VALID.load(Ordering::Acquire) {
        let lat = POSITION_LAT.load(Ordering::Acquire);
        let lon = POSITION_LON.load(Ordering::Acquire);
        let lat = (lat * 1e16) as i64;
        let lon = (lon * 1e16) as i64;
        return (Some(lat), Some(lon));
    }
    return (None, None);
}

pub(crate) struct NavigationData {
    args: Cli,
}

impl NavigationData {
    pub(crate) fn new(args: Cli) -> Self {
        NavigationData { args }
    }

    pub(crate) async fn run(
        &self,
        subsys: SubsystemHandle,
        rx_ip_change: Receiver<()>,
    ) -> Result<(), Error> {
        log::debug!("SignalK run_loop (re)start");
        let mut rx_ip_change = rx_ip_change;
        loop {
            match self.find_service(&subsys, &mut rx_ip_change).await {
                Ok(stream) => {
                    log::info!(
                        "Listening to Signal K data from {}",
                        stream.peer_addr().unwrap()
                    );
                    match self.receive_loop(stream, &subsys).await {
                        Err(RadarError::Shutdown) => {
                            log::debug!("Signal K receive_loop shutdown");
                            return Ok(());
                        }
                        e => {
                            log::debug!("Signal K receive_loop restart on result {:?}", e);
                        }
                    }
                }
                Err(e) => match e {
                    RadarError::Shutdown => {
                        log::debug!("Signal K run_loop shutdown");
                        return Ok(());
                    }
                    e => {
                        log::debug!("Signal K find_service restart on result {:?}", e);
                    }
                },
            }
        }
    }

    async fn find_service(
        &self,
        subsys: &SubsystemHandle,
        rx_ip_change: &mut Receiver<()>,
    ) -> Result<TcpStream, RadarError> {
        let mut known_addresses: HashSet<SocketAddr> = HashSet::new();

        let mdns = ServiceDaemon::new().expect("Failed to create daemon");

        if self.args.signalk_interface.is_some() {
            let _ = mdns.disable_interface(IfKind::All);
            let _ = mdns.enable_interface(IfKind::Name(
                self.args.signalk_interface.as_ref().unwrap().to_string(),
            ));
        }
        let tcp_locator = mdns
            .browse(TCP_SERVICE_NAME)
            .expect("Failed to browse for Signal K service");

        log::debug!("SignalK find_service (re)start");

        let r: Result<TcpStream, RadarError>;
        loop {
            let s = &subsys;
            tokio::select! { biased;
                _ = s.on_shutdown_requested() => {
                    r = Err(RadarError::Shutdown);
                    break;
                },
                _ = rx_ip_change.recv() => {
                    log::debug!("rx_ip_change");
                    r = Err(RadarError::IPAddressChanged);
                    break;
                },
                event = tcp_locator.recv_async() => {
                    match event {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            log::debug!("Resolved a new SignalK service: {}", info.get_fullname());
                            let addr = info.get_addresses();
                            let port = info.get_port();

                            for a in addr {
                                known_addresses.insert(SocketAddr::new(*a, port));
                            }
                        },
                        _ => {
                            continue;
                        }
                    }

                }
            }

            let stream = connect_first(known_addresses.clone()).await;
            match stream {
                Ok(stream) => {
                    log::info!(
                        "Listening to Signal K data from {}",
                        stream.peer_addr().unwrap()
                    );

                    r = Ok(stream);
                    break;
                }
                Err(_e) => {} // Just loop
            }
        }

        log::debug!("Signal K find_service end with {:?}", r);
        if let Ok(r3) = mdns.shutdown() {
            if let Ok(r3) = r3.recv() {
                log::debug!("mdns_shutdown: {:?}", r3);
            }
        }
        return r;
    }

    // Loop until we get an error, then just return the error
    // or Ok if we are to shutdown.
    async fn receive_loop(
        &self,
        mut stream: TcpStream,
        subsys: &SubsystemHandle,
    ) -> Result<(), RadarError> {
        let (read_half, mut write_half) = stream.split();
        let mut lines = BufReader::new(read_half).lines();

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("SK receive_loop shutdown");
                    return Ok(());
                },
                r = lines.next_line() => {
                    match r {
                        Ok(Some(line)) => {
                            log::trace!("SK <- {}", line);
                            if line.starts_with("{\"name\":") {
                                self.send_subscription(&mut write_half).await?;
                                log::trace!("SK -> {}", SUBSCRIBE);
                            }
                            else {
                                match parse_signalk(&line) {
                                    Err(e) => { log::warn!("{}", e)}
                                    Ok(_) => { }
                                }
                            }
                        }
                        Ok(None) => {
                            return Ok(());
                        }
                        Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(e) => {
                            return Err(e.into());
                        }
                    }
                }
            }
        }
    }

    async fn send_subscription(
        &self,
        stream: &mut tokio::net::tcp::WriteHalf<'_>,
    ) -> Result<(), RadarError> {
        let bytes: &[u8] = SUBSCRIBE.as_bytes();

        stream.write_all(bytes).await.map_err(|e| RadarError::Io(e))
    }
}

//  {"context":"vessels.urn:mrn:imo:mmsi:244060807","updates":
//   [{"source":{"sentence":"GLL","talker":"BM","type":"NMEA0183","label":"canboat-merrimac"},
//     "$source":"canboat-merrimac.BM","timestamp":"2024-10-01T09:11:36.000Z",
//     "values":[{"path":"navigation.position","value":{"longitude":5.428445,"latitude":53.180205}}]}]}

fn parse_signalk(s: &str) -> Result<(), RadarError> {
    match serde_json::from_str::<Value>(s) {
        Ok(v) => {
            let updates = &v["updates"][0];
            let values = &updates["values"][0];
            {
                log::trace!("values: {:?}", values);

                if let (Some(path), value) = (values["path"].as_str(), &values["value"]) {
                    match path {
                        "navigation.position" => {
                            if let (Some(longitude), Some(latitude)) =
                                (value["longitude"].as_f64(), value["latitude"].as_f64())
                            {
                                POSITION_VALID.store(true, Ordering::Release);
                                POSITION_LON.store(longitude, Ordering::Release);
                                POSITION_LAT.store(latitude, Ordering::Release);
                                return Ok(());
                            }
                        }
                        "navigation.headingTrue" => {
                            if let Some(heading) = value.as_f64() {
                                HEADING_TRUE_VALID.store(true, Ordering::Release);
                                HEADING_TRUE.store(heading, Ordering::Release);
                                return Ok(());
                            }
                        }
                        _ => {
                            return Err(RadarError::ParseJson(format!("Ignored path '{}'", path)));
                        }
                    }
                }
            }
        }
        Err(e) => {
            log::warn!("Unable to parse SK message '{}'", s);
            return Err(RadarError::ParseJson(e.to_string()));
        }
    }
    return Err(RadarError::ParseJson(format!(
        "Insufficient fields in '{}'",
        s
    )));
}

async fn connect_to_socket(address: SocketAddr) -> Result<TcpStream, RadarError> {
    let stream = TcpStream::connect(address)
        .await
        .map_err(|e| RadarError::Io(e))?;
    log::debug!("Connected to {}", address);
    Ok(stream)
}

///
/// Take an interable of SocketAddr and return a TCP stream to the first socket that connects.
///
async fn connect_first<I>(addresses: I) -> Result<TcpStream, RadarError>
where
    I: IntoIterator<Item = SocketAddr>,
{
    // Create a collection of connection futures
    // Since the life time of the stream must outlive this function,
    // and we create async closures on the stack, we must add a lot
    // of syntactic sugar so the compiler doesn't grumble.
    // Future<....> says that it is async, e.g. first call returns a future.
    // It resolves to Output = ... and is Send.
    // Box<> places this on the heap, not stack.
    // Pin<> makes sure it doesn't move or get invalid as an object.
    // Vec<> so we can store a list of these.
    let futures: Vec<Pin<Box<dyn Future<Output = Result<TcpStream, RadarError>> + Send>>> =
        addresses
            .into_iter()
            .map(|address| {
                log::debug!("Connecting to {}", address);
                Box::pin(connect_to_socket(address)) as Pin<Box<dyn Future<Output = _> + Send>>
            })
            .collect();

    // Use select_ok to return the first successful connection
    match select_ok(futures).await {
        Ok((stream, _)) => {
            log::debug!("First successful connection: {:?}", stream);
            Ok(stream)
        }
        Err(e) => {
            log::debug!("All connections failed: {}", e);
            Err(e)
        }
    }
}
