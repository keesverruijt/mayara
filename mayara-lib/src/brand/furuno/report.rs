use anyhow::{bail, Context, Error};
use num_traits::FromPrimitive;
use protobuf::Message;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::net::UdpSocket;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::{Command, CommandId};
use super::settings;
use super::RadarModel;
use super::{FURUNO_DATA_BROADCAST_ADDRESS, FURUNO_SPOKE_LEN};
use crate::network::{create_udp_listen, create_udp_multicast_listen};
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::trail::TrailBuffer;
use crate::radar::CommonRadar;
use crate::radar::SpokeBearing;
use crate::radar::{RadarError, RadarInfo, Status};
use crate::settings::ControlType;
use crate::util::PrintableSpoke;
use crate::Session;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ReceiveAddressType {
    Both,
    Multicast,
    Broadcast,
}

#[derive(Debug)]
struct FurunoSpokeMetadata {
    sweep_count: u32,
    sweep_len: u32,
    encoding: u8,
    have_heading: u8,
    range: u32,
}

pub struct FurunoReportReceiver {
    common: CommonRadar,
    session: Session,
    stream: Option<TcpStream>,
    command_sender: Option<Command>,
    report_request_interval: Duration,
    model_known: bool,

    receive_type: ReceiveAddressType,
    multicast_socket: Option<UdpSocket>,
    broadcast_socket: Option<UdpSocket>,

    // pixel_to_blob: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH],
    prev_spoke: Vec<u8>,
    prev_angle: u16,
    sweep_count: u16,
}

impl FurunoReportReceiver {
    pub fn new(session: Session, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();

        let args = session.read().unwrap().args.clone();
        let replay = args.replay;
        let radars = session.read().unwrap().radars.clone().unwrap();
        let command_sender = if replay {
            Some(Command::new(&info))
        } else {
            None
        };

        let control_update_rx = info.controls.control_update_subscribe();

        // let pixel_to_blob = Self::pixel_to_blob(&info.legend);
        let mut trails = TrailBuffer::new(session.clone(), &info);
        if let Some(control) = info.controls.get(&ControlType::DopplerTrailsOnly) {
            if let Some(value) = control.value {
                let value = value > 0.;
                trails.set_doppler_trail_only(value);
            }
        }

        let common = CommonRadar::new(key, info, radars, trails, control_update_rx, replay);

        FurunoReportReceiver {
            common,
            session,
            stream: None,
            command_sender,
            report_request_interval: Duration::from_millis(5000),
            model_known: false,
            receive_type: ReceiveAddressType::Both,
            multicast_socket: None,
            broadcast_socket: None,
            prev_spoke: Vec::new(),
            prev_angle: 0,
            sweep_count: 0,
        }
    }

    async fn start_command_stream(&mut self) -> Result<(), RadarError> {
        if self.common.info.send_command_addr.port() == 0 {
            // Port not set yet, we need to login to the radar first.
            return Err(RadarError::InvalidPort);
        }
        let sock = TcpSocket::new_v4().map_err(|e| RadarError::Io(e))?;
        self.stream = Some(
            sock.connect(std::net::SocketAddr::V4(self.common.info.send_command_addr))
                .await
                .map_err(|e| RadarError::Io(e))?,
        );
        Ok(())
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);
        let mut command_rx = self.common.info.control_update_subscribe();

        let stream = self.stream.take().unwrap();
        let (reader, mut writer) = tokio::io::split(stream);
        if let Some(ref mut cs) = self.command_sender {
            cs.set_writer(writer);
        }
        // self.common.command_sender.init(&mut writer).await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let mut deadline = Instant::now() + self.report_request_interval;
        let mut first_report_received = false;

        let mut buf = Vec::with_capacity(9000);
        let mut buf2 = Vec::with_capacity(9000);

        let mut multicast_socket = self.multicast_socket.take();
        let mut broadcast_socket = self.broadcast_socket.take();

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(deadline) => {
                    if let Some(cs) = &mut self.command_sender {
                        cs.send_report_requests().await?;
                    }
                    deadline = Instant::now() + self.report_request_interval;
                },

                r = reader.read_line(&mut line) => {
                    match r {
                        Ok(len) => {
                            if len > 2 {
                                if let Err(e) = self.process_report(&line) {
                                    log::error!("{}: {}", self.common.key, e);
                                } else if !first_report_received {
                                    if let Some(ref mut cs) = self.command_sender {
                                        cs.init().await?;
                                    }
                                    first_report_received = true;
                                }
                            }
                            line.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Err(e) = self.common.process_control_update( cv, &mut self.command_sender).await {
                                return Err(e);
                            }
                        },
                    }
                },

                Some(r) = Self::conditional_receive(&multicast_socket, &mut buf)  => {
                    log::trace!("Furuno data multicast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                self.process_frame(&buf[..len]);
                                self.receive_type = ReceiveAddressType::Multicast;
                                broadcast_socket = None;
                            }
                        },
                        Err(e) => {
                            log::error!("Furuno data socket: {}", e);
                            return Err(RadarError::Io(e));
                        }
                    };
                    buf.clear();
                },

                Some(r) = Self::conditional_receive(&broadcast_socket, &mut buf2)  => {
                    log::trace!("Furuno data broadcast recv {:?}", r);
                    match r {
                        Ok((len, addr)) => {
                            if self.verify_source_address(&addr) {
                                self.process_frame(&buf2[..len]);
                                self.receive_type = ReceiveAddressType::Broadcast;
                                multicast_socket = None;
                            }
                        },
                        Err(e) => {
                            log::error!("Furuno data socket: {}", e);
                            return Err(RadarError::Io(e));
                        }
                    };
                    buf2.clear();
                },

            }
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_data_socket().await?;
        self.start_command_stream().await?;
        loop {
            if self.stream.is_some() {
                match self.data_loop(&subsys).await {
                    Err(RadarError::Shutdown) => {
                        return Ok(());
                    }
                    _ => {
                        // Ignore, reopen socket
                    }
                }
                self.stream = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.login_to_radar()?;
                self.start_command_stream().await?;
                self.start_data_socket().await?;
            }
        }
    }

    fn login_to_radar(&mut self) -> Result<(), RadarError> {
        // Furuno radars use a single TCP/IP connection to send commands and
        // receive status reports, so report_addr and send_command_addr are identical.
        // Only one of these would be enough for Furuno.
        let port: u16 = match super::login_to_radar(self.session.clone(), self.common.info.addr) {
            Err(e) => {
                log::error!(
                    "{}: Unable to connect for login: {}",
                    self.common.info.key(),
                    e
                );
                return Err(RadarError::LoginFailed);
            }
            Ok(p) => p,
        };
        if port != self.common.info.send_command_addr.port() {
            self.common.info.send_command_addr.set_port(port);
            self.common.info.report_addr.set_port(port);
        }
        Ok(())
    }

    fn set(&mut self, control_type: &ControlType, value: f32, auto: Option<bool>) {
        match self.common.info.controls.set(control_type, value, auto) {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.common.info.controls.get(control_type).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} enabled {:?}",
                        self.common.key,
                        control_type,
                        control.value(),
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_value(&mut self, control_type: &ControlType, value: f32) {
        self.set(control_type, value, None)
    }

    fn set_value_auto(&mut self, control_type: &ControlType, value: f32, auto: u8) {
        match self
            .common
            .info
            .controls
            .set_value_auto(control_type, auto > 0, value)
        {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.common.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto {}",
                        self.common.key,
                        control_type,
                        control.value(),
                        auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    #[allow(dead_code)]
    fn set_value_with_many_auto(
        &mut self,
        control_type: &ControlType,
        value: f32,
        auto_value: f32,
    ) {
        match self
            .common
            .info
            .controls
            .set_value_with_many_auto(control_type, value, auto_value)
        {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.common.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto_value {:?} auto {:?}",
                        self.common.key,
                        control_type,
                        control.value(),
                        control.auto_value,
                        control.auto
                    );
                }
            }
            Ok(None) => {}
        };
    }

    #[allow(dead_code)]
    fn set_string(&mut self, control: &ControlType, value: String) {
        match self.common.info.controls.set_string(control, value) {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(v)) => {
                log::debug!(
                    "{}: Control '{}' new value '{}'",
                    self.common.key,
                    control,
                    v
                );
            }
            Ok(None) => {}
        };
    }

    fn process_report(&mut self, line: &str) -> Result<(), Error> {
        let line = match line.find('$') {
            Some(pos) => {
                if pos > 0 {
                    log::warn!(
                        "{}: Ignoring first {} bytes of TCP report",
                        self.common.key,
                        pos
                    );
                    &line[pos..]
                } else {
                    line
                }
            }
            None => {
                log::warn!("{}: TCP report dropped, no $", self.common.key);
                return Ok(());
            }
        };

        if line.len() < 2 {
            bail!("TCP report {:?} dropped", line);
        }
        let (prefix, mut line) = line.split_at(2);
        if prefix != "$N" {
            bail!("TCP report {:?} dropped", line);
        }
        line = line.trim_end_matches("\r\n");

        log::trace!("{}: processing $N{}", self.common.key, line);

        let mut values_iter = line.split(',');

        let cmd_str = values_iter
            .next()
            .ok_or(io::Error::new(io::ErrorKind::Other, "No command ID"))?;
        let cmd = u8::from_str_radix(cmd_str, 16)?;

        let command_id = match CommandId::from_u8(cmd) {
            Some(c) => c,
            None => {
                log::debug!(
                    "{}: ignoring unimplemented command {}",
                    self.common.key,
                    cmd_str
                );
                return Ok(());
            }
        };

        // Match commands that do not have just numbers as arguments first

        let strings: Vec<&str> = values_iter.collect();
        log::debug!(
            "{}: command {:02X} strings {:?}",
            self.common.key,
            cmd,
            strings
        );
        let numbers: Vec<f32> = strings
            .iter()
            .map(|s| s.trim().parse::<f32>().unwrap_or(0.0))
            .collect();

        if numbers.len() != strings.len() {
            log::trace!("Parsed strings: $N{:02X},{:?}", cmd, strings);
        } else {
            log::trace!("Parsed numbers: $N{:02X},{:?}", cmd, numbers);
        }

        match command_id {
            CommandId::Modules => {
                self.parse_modules(&strings);
                return Ok(());
            }

            CommandId::Status => {
                if numbers.len() < 1 {
                    bail!("No arguments for Status command");
                }
                let generic_state = match numbers[0] {
                    0. => Status::Preparing,
                    1. => Status::Standby,
                    2. => Status::Transmit,
                    3. => Status::Off,
                    _ => Status::Off,
                };
                // TODO check values with generic values [1 = Standby, 2 = Transmit but the others...]
                self.set_value(&ControlType::Status, generic_state as i32 as f32);
            }
            CommandId::Gain => {
                if numbers.len() < 5 {
                    bail!(
                        "Insufficient ({}) arguments for Gain command",
                        numbers.len()
                    );
                }
                let auto = numbers[2] as u8;
                let gain = if auto > 0 { numbers[3] } else { numbers[1] };
                log::trace!(
                    "Gain: {} auto {} values[1]={} values[3]={}",
                    gain,
                    auto,
                    numbers[1],
                    numbers[3]
                );
                self.set_value_auto(&ControlType::Gain, gain, auto);
            }
            CommandId::Range => {
                if numbers.len() < 3 {
                    bail!(
                        "Insufficient ({}) arguments for Range command",
                        numbers.len()
                    );
                }
                if numbers[2] != 0. {
                    bail!("Cannot handle radar not set to NM range");
                }
                let index = numbers[0] as usize;
                let range = self.common.info.ranges.all.get(index).with_context(|| {
                    format!(
                        "Range index {} out of bounds for ranges {}",
                        index, self.common.info.ranges,
                    )
                })?;

                self.set_value(&ControlType::Range, range.distance() as f32);
            }
            CommandId::OnTime => {
                let hours = numbers[0] / 3600.0;
                self.set_value(&ControlType::OperatingHours, hours);
            }
            CommandId::AliveCheck => {}
            _ => {
                bail!("TODO: Handle command {:?} values {:?}", command_id, numbers);
            }
        }
        Ok(())
    }

    /// Parse the connect reply from the radar.
    /// The DRS 4D-NXT radar sends a connect reply with the following format:
    /// $N96,0359360-01.05,0359358-01.01,0359359-01.01,0359361-01.05,,,
    /// The 4th, 5th and 6th values are for the FPGA and other parts, we don't store
    /// that (yet).
    fn parse_modules(&mut self, values: &Vec<&str>) {
        if self.model_known {
            return;
        }
        self.model_known = true; // We set this even if we can't parse the model, there is no point in logging errors many times.

        if let Some((model, version)) = values[0].split_once('-') {
            let model = Self::parse_model(model);
            log::info!(
                "{}: Radar model {} version {}",
                self.common.key,
                model.to_str(),
                version
            );
            settings::update_when_model_known(&mut self.common.info, model, version);
            if let Some(cs) = &mut self.command_sender {
                cs.set_ranges(self.common.info.ranges.clone());
            }
            return;
        }
        log::error!(
            "{}: Unknown radar type, modules {:?}",
            self.common.key,
            values
        );
    }

    // See TZ Fec.Wrapper.SensorProperty.GetRadarSensorType
    fn parse_model(model: &str) -> RadarModel {
        match model {
            "0359235" => RadarModel::DRS,
            "0359255" => RadarModel::FAR14x7,
            "0359204" => RadarModel::FAR21x7,
            "0359321" => RadarModel::FAR14x7,
            "0359338" => RadarModel::DRS4DL,
            "0359367" => RadarModel::DRS4DL,
            "0359281" => RadarModel::FAR3000,
            "0359286" => RadarModel::FAR3000,
            "0359477" => RadarModel::FAR3000,
            "0359360" => RadarModel::DRS4DNXT,
            "0359421" => RadarModel::DRS6ANXT,
            "0359355" => RadarModel::DRS6AXCLASS,
            "0359344" => RadarModel::FAR15x3,
            "0359397" => RadarModel::FAR14x6,

            _ => RadarModel::Unknown, // Default case
        }
    }

    async fn start_multicast_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(
            &self.common.info.spoke_data_addr,
            &self.common.info.nic_addr,
        ) {
            Ok(sock) => {
                self.multicast_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr
                );
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{} via {}: listen multicast failed: {}",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr,
                    e
                );
            }
        };
        Ok(())
    }

    async fn start_broadcast_socket(&mut self) -> io::Result<()> {
        match create_udp_listen(
            &FURUNO_DATA_BROADCAST_ADDRESS,
            &self.common.info.nic_addr,
            true,
        ) {
            Ok(sock) => {
                self.broadcast_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &FURUNO_DATA_BROADCAST_ADDRESS,
                    &self.common.info.nic_addr
                );
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{} via {}: listen broadcast failed: {}",
                    &FURUNO_DATA_BROADCAST_ADDRESS,
                    &self.common.info.nic_addr,
                    e
                );
            }
        };
        Ok(())
    }

    async fn start_data_socket(&mut self) -> io::Result<()> {
        if self.receive_type != ReceiveAddressType::Broadcast && self.multicast_socket.is_none() {
            self.start_multicast_socket().await?;
        }
        if self.receive_type != ReceiveAddressType::Multicast && self.broadcast_socket.is_none() {
            self.start_broadcast_socket().await?;
        }

        Ok(())
    }

    async fn conditional_receive(
        socket: &Option<UdpSocket>,
        buf: &mut Vec<u8>,
    ) -> Option<io::Result<(usize, SocketAddr)>> {
        match socket {
            Some(s) => Some(s.recv_buf_from(buf).await),
            None => None,
        }
    }

    #[cfg(target_os = "macos")]
    fn verify_source_address(&self, addr: &SocketAddr) -> bool {
        addr.ip() == std::net::SocketAddr::V4(self.common.info.addr).ip()
            || self.session.read().unwrap().args.replay
    }
    #[cfg(not(target_os = "macos"))]
    fn verify_source_address(&self, addr: &SocketAddr) -> bool {
        addr.ip() == std::net::SocketAddr::V4(self.common.info.addr).ip()
    }

    fn process_frame(&mut self, data: &[u8]) {
        if data.len() < 16 || data[0] != 0x02 {
            log::debug!("Dropping invalid frame");
            return;
        }

        let mut message = RadarMessage::new();
        message.radar = self.common.info.id as u32;

        let metadata: FurunoSpokeMetadata = self.parse_metadata_header(&data);

        let sweep_count = metadata.sweep_count;
        let sweep_len = metadata.sweep_len as usize;
        log::debug!(
            "Received UDP frame with {} spokes, total {}",
            sweep_count,
            self.sweep_count
        );

        let mut message = RadarMessage::new();
        message.radar = self.common.info.id as u32;

        let mut sweep: &[u8] = &data[16..];
        for sweep_idx in 0..sweep_count {
            if sweep.len() < 5 {
                log::error!("Unsufficient data for sweep {}", sweep_idx);
                break;
            }
            let angle = ((sweep[1] as u16) << 8) | sweep[0] as u16;
            let heading = ((sweep[3] as u16) << 8) | sweep[2] as u16;
            sweep = &sweep[4..];

            let (generic_spoke, used) = match metadata.encoding {
                0 => Self::decode_sweep_encoding_0(sweep),
                1 => Self::decode_sweep_encoding_1(sweep, sweep_len),
                2 => {
                    if sweep_idx == 0 {
                        Self::decode_sweep_encoding_1(sweep, sweep_len)
                    } else {
                        Self::decode_sweep_encoding_2(sweep, self.prev_spoke.as_slice(), sweep_len)
                    }
                }
                3 => Self::decode_sweep_encoding_3(sweep, self.prev_spoke.as_slice(), sweep_len),
                _ => {
                    panic!("Impossible encoding value")
                }
            };
            sweep = &sweep[used..];

            message
                .spokes
                .push(self.create_spoke(&metadata, angle, heading, &generic_spoke));

            self.sweep_count += 1;
            if angle < self.prev_angle {
                let ms = self.common.info.full_rotation();
                self.common.trails.set_rotation_speed(ms);

                log::debug!("sweep_count = {}", self.sweep_count);
                if log::log_enabled!(log::Level::Debug) {
                    let _ = self
                        .common
                        .info
                        .controls
                        .set_string(&ControlType::Spokes, sweep_count.to_string());
                }
                self.sweep_count = 0;
            }
            self.prev_angle = angle;
            self.prev_spoke = generic_spoke;
        }

        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");

        if !log::log_enabled!(log::Level::Debug) {
            match self.common.info.message_tx.send(bytes) {
                Err(e) => {
                    log::trace!("{}: Dropping received spoke: {}", self.common.key, e);
                }
                Ok(count) => {
                    log::trace!("{}: sent to {} receivers", self.common.key, count);
                }
            }
        }
    }

    fn decode_sweep_encoding_0(sweep: &[u8]) -> (Vec<u8>, usize) {
        let spoke = sweep.to_vec();

        let used = sweep.len();
        (spoke, used)
    }

    fn decode_sweep_encoding_1(sweep: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 1;
                if repeat == 0 {
                    repeat = 0x80;
                }

                for _ in 0..repeat {
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn decode_sweep_encoding_2(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                let strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 1;
                if repeat == 0 {
                    repeat = 0x80;
                }

                for _ in 0..repeat {
                    let i = spoke.len();
                    let strength = if prev_spoke.len() > i {
                        prev_spoke[i]
                    } else {
                        0
                    };
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn decode_sweep_encoding_3(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x03 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 2;
                if repeat == 0 {
                    repeat = 0x40;
                }

                if sweep[used] & 0x01 == 0 {
                    for _ in 0..repeat {
                        let i = spoke.len();
                        strength = if prev_spoke.len() > i {
                            prev_spoke[i]
                        } else {
                            0
                        };
                        spoke.push(strength);
                    }
                } else {
                    for _ in 0..repeat {
                        spoke.push(strength);
                    }
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn create_spoke(
        &mut self,
        metadata: &FurunoSpokeMetadata,
        angle: SpokeBearing,
        heading: SpokeBearing,
        sweep: &[u8],
    ) -> Spoke {
        if self.session.read().unwrap().args.replay {
            let _ = self
                .common
                .info
                .controls
                .set(&ControlType::Range, metadata.range as f32, None);
        }
        // Convert the spoke data to bytes

        let heading: Option<u32> = if metadata.have_heading > 0 {
            Some(heading as u32)
        } else {
            let heading = crate::navdata::get_heading_true();
            heading.map(|h| h as u32)
        };

        let mut spoke = Spoke::new();
        spoke.range = metadata.range;
        //        spoke.angle = (angle as usize * FURUNO_SPOKES / 8192) as u32;
        spoke.angle = angle as u32;
        spoke.bearing = heading;

        (spoke.lat, spoke.lon) = crate::navdata::get_position_i64();
        spoke.time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok();

        spoke.data = vec![0; sweep.len()];

        let mut i = 0;
        for b in sweep {
            spoke.data[i] = b >> 2;
            i += 1;
        }
        if self.session.read().unwrap().args.replay {
            spoke.data[sweep.len() - 1] = 64;
        }

        log::trace!(
            "Received {:04}/{:04} spoke {}",
            angle,
            heading.unwrap_or(99999),
            PrintableSpoke::new(&spoke.data)
        );

        self.common
            .trails
            .update_trails(&mut spoke, &self.common.info.legend);

        spoke
    }

    // From RadarDLLAccess RmGetEchoData() we know that the following should be in the header:
    // status, sweep_len, scale, range, angle, heading, hdg_flag.
    //
    // derived from ghidra fec/radar.dll function 'decode_sweep_2' @ 10002740
    // called from DecodeImoEchoFormat
    // Here's a typical header:
    //  [2,    #  0: 0x02 - Always 2, checked in radar.dll
    //   149,  #  1: 0x95
    //   0,
    //   1,
    //   0, 0, 0, 0,
    //   48,   #  8: 0x30 - low byte of range? (= range * 4 + 4)
    //   17,   #  9: 0x11 - bit 0 = high bit of range
    //   116,  # 10: 0x74 - low byte of sweep_len
    //   219,  # 11: 0xDB - bits 2..0 (011) = bits 10..8 of sweep_len
    //                    - bits 4..3 (11) = encoding 3
    //                    - bits 7..5 (110) = ?
    //   6,    # 12: 0x06
    //   0,    # 13: 0x00
    //   240,  # 14: 0xF0
    //   9]    # 15: 0x09
    //
    //  multi byte data: sweep_len = 0b011 << 8 | 0x74 => 0x374 = 884

    //  -> sweep_count=8 sweep_len=884 encoding=3 have_heading=0 range=496

    // Some more headers from FAR-2127:
    // [2, 250, 0, 1, 0, 0, 0, 0, 36, 49, 116, 59, 0, 0, 240, 9]

    fn parse_metadata_header(&self, data: &[u8]) -> FurunoSpokeMetadata {
        let ranges = &self.common.info.ranges;

        // Extract all the fields from the header
        let v1 = (data[8] as u32 + (data[9] as u32 & 0x01) * 256) * 4 + 4;
        let sweep_count = (data[9] >> 1) as u32;
        let sweep_len = ((data[11] & 0x07) as u32) << 8 | data[10] as u32;
        let encoding = (data[11] & 0x18) >> 3;
        let v2 = (data[11] & 0x20) >> 5;
        let v3 = (data[11] & 0xc0) >> 6;
        let range_index = data[12] as usize;
        let have_heading = ((data[15] & 0x30) >> 3) as u8;

        // Now do stuff with the data
        let range = ranges
            .all
            .get(range_index)
            .map(|r| r.distance())
            .unwrap_or_else(|| {
                log::warn!(
                    "Unknown range index {} in header: {:?}",
                    range_index,
                    &data[0..20]
                );
                0
            });
        let range = range as u32;
        let metadata = FurunoSpokeMetadata {
            sweep_count,
            sweep_len,
            encoding,
            have_heading,
            range,
        };
        if self.sweep_count < self.prev_angle {
            log::debug!(
                "header {:?} -> v1={v1}, v2={v2}, v3={v3}, sweep_count={} sweep_len={} encoding={} have_heading={} range={}",
                &data[0..20],
                sweep_count,
                sweep_len,
                encoding,
                have_heading,
                range
            );
        }

        metadata
    }
}
