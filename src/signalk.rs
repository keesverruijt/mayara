use atomic_float::AtomicF64;
use futures_util::future::select_ok;
use mdns_sd::{Error, ServiceDaemon, ServiceEvent};
use serde_json::Value;
use std::{
    collections::HashSet,
    future::Future,
    io::ErrorKind,
    net::SocketAddr,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::{io::AsyncWriteExt, net::TcpStream};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::RadarError;

/// The hostname of the devices we are searching for.
/// Every Chromecast will respond to the service name in this example.
const TCP_SERVICE_NAME: &'static str = "_signalk-tcp._tcp.local.";

const SUBSCRIBE: &'static str =
    "{\"context\": \"vessels.self\",\"subscribe\": [{\"path\": \"navigation.headingTrue\"},{\"path\": \"navigation.position\"}]}\r\n";

pub(crate) static HEADING_TRUE_VALID: AtomicBool = AtomicBool::new(false);
pub(crate) static POSITION_VALID: AtomicBool = AtomicBool::new(false);
pub(crate) static HEADING_TRUE: AtomicF64 = AtomicF64::new(0.0);
pub(crate) static POSITION_LAT: AtomicF64 = AtomicF64::new(0.0);
pub(crate) static POSITION_LON: AtomicF64 = AtomicF64::new(0.0);

pub(crate) struct NavigationData {}

impl NavigationData {
    pub(crate) fn new() -> Self {
        NavigationData {}
    }

    pub(crate) async fn run(self, subsys: SubsystemHandle) -> Result<(), Error> {
        let mdns = ServiceDaemon::new().expect("Failed to create daemon");
        let mut known_addresses: HashSet<SocketAddr> = HashSet::new();

        let tcp_locator = mdns.browse(TCP_SERVICE_NAME)?;
        // let ws_receiver = mdns.browse(WS_SERVICE_NAME)?;

        loop {
            let s = &subsys;
            POSITION_VALID.store(false, Ordering::Release);
            HEADING_TRUE_VALID.store(false, Ordering::Release);

            tokio::select! { biased;
                _ = s.on_shutdown_requested() => {
                    break;
                },
                event = tcp_locator.recv_async() => {
                    match event {
                        Ok(ServiceEvent::ServiceResolved(info)) => {
                            log::info!("Resolved a new service: {}", info.get_fullname());
                            let addr = info.get_addresses();
                            let port = info.get_port();

                            for a in addr {
                                known_addresses.insert(SocketAddr::new(*a, port));
                            }
                        },
                        other_event => {
                            log::info!("Received other event: {:?}", &other_event);
                            continue;
                        }
                    }

                }
            }

            let stream = connect_first(known_addresses.clone()).await;
            match stream {
                Ok(stream) => {
                    if self.receive_loop(stream, &subsys).await.is_ok() {
                        break;
                    }
                }
                Err(_e) => {} // Just loop
            }
        }

        mdns.shutdown().unwrap();
        return Ok(());
    }

    // Loop until we get an error, then just return the error
    // or Ok if we are to shutdown.
    async fn receive_loop(
        &self,
        mut stream: TcpStream,
        subsys: &SubsystemHandle,
    ) -> Result<(), RadarError> {
        loop {
            let mut buf = [0; 4096];

            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Ok(());
                },
                _ = stream.readable() => {
                    match stream.try_read(&mut buf) {
                        Ok(n) => {
                            if n > 2 {

                                // Try to convert to String
                                if let Ok(s) = std::str::from_utf8(&buf[0..n]) {
                                    log::info!("SK <- {}", s);
                                    let s = s.to_string();
                                    if s.starts_with("{\"name\":") {
                                        self.subscribe(&mut stream).await?;
                                        log::info!("SK -> {}", SUBSCRIBE);
                                    }
                                    else {
                                        match parse_signalk(&s) {
                                            Err(e) => { log::warn!("{}", e)}
                                            Ok(_) => { }
                                        }
                                    }
                                }
                            }
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

    async fn subscribe(&self, stream: &mut TcpStream) -> Result<(), RadarError> {
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
            let values = &v["values"];
            {
                log::info!("{:?}", values);

                if let (Some(path), Some(value)) =
                    (values["path"].as_str(), values["value"].as_object())
                {
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
                        "navigation.trueHeading" => {
                            if let Some(heading) = value["heading"].as_f64() {
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
    log::info!("Connected to {}", address);
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
                log::info!("Connecting to {}", address);
                Box::pin(connect_to_socket(address)) as Pin<Box<dyn Future<Output = _> + Send>>
            })
            .collect();

    // Use select_ok to return the first successful connection
    match select_ok(futures).await {
        Ok((stream, _)) => {
            log::info!("First successful connection: {:?}", stream);
            Ok(stream)
        }
        Err(e) => {
            log::info!("All connections failed: {}", e);
            Err(e)
        }
    }
}
