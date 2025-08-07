use anyhow::{bail, Context, Error};
use num_traits::FromPrimitive;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

use std::io;
use std::time::Duration;
use tokio::io::WriteHalf;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::CommandId;
use super::settings;
use super::RadarModel;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlUpdate};
use crate::Session;

use super::command::Command;

pub struct FurunoReportReceiver {
    session: Session,
    info: RadarInfo,
    key: String,
    command_sender: Command,
    stream: Option<TcpStream>,
    report_request_interval: Duration,
    model_known: bool,
}

impl FurunoReportReceiver {
    pub fn new(session: Session, info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();

        let command_sender = Command::new(&info);

        FurunoReportReceiver {
            session,
            info,
            key,
            command_sender,
            stream: None,
            report_request_interval: Duration::from_millis(5000),
            model_known: false,
        }
    }

    async fn start_stream(&mut self) -> Result<(), RadarError> {
        if self.info.send_command_addr.port() == 0 {
            // Port not set yet, we need to login to the radar first.
            return Err(RadarError::InvalidPort);
        }
        let sock = TcpSocket::new_v4().map_err(|e| RadarError::Io(e))?;
        self.stream = Some(
            sock.connect(std::net::SocketAddr::V4(self.info.send_command_addr))
                .await
                .map_err(|e| RadarError::Io(e))?,
        );
        Ok(())
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.info.command_tx.
    //
    async fn data_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.key);
        let mut command_rx = self.info.control_update_subscribe();

        let stream = self.stream.take().unwrap();
        let (reader, mut writer) = tokio::io::split(stream);
        // self.command_sender.init(&mut writer).await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let mut deadline = Instant::now() + self.report_request_interval;
        let mut first_report_received = false;

        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(deadline) => {
                    self.command_sender.send_report_requests(&mut writer).await?;
                    deadline = Instant::now() + self.report_request_interval;
                },

                r = reader.read_line(&mut line) => {
                    match r {
                        Ok(len) => {
                            if len > 2 {
                                if let Err(e) = self.process_report(&line) {
                                    log::error!("{}: {}", self.key, e);
                                } else if !first_report_received {
                                    self.command_sender.init(&mut writer).await?;
                                    first_report_received = true;
                                }
                            }
                            line.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Err(e) = self.process_control_update(&mut writer, cv).await {
                                return Err(e);
                            }
                        },
                    }
                }
            }
        }
    }

    async fn process_control_update(
        &mut self,
        write: &mut WriteHalf<TcpStream>,
        control_update: ControlUpdate,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        if let Err(e) = self.command_sender.set_control(write, &cv).await {
            self.info
                .controls
                .send_error_to_client(reply_tx, &cv, &e)
                .await?;
            match &e {
                RadarError::Io(_) => {
                    return Err(e);
                }
                _ => {}
            }
        } else {
            self.info.controls.set_refresh(&cv.id);
        }

        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_stream().await?;
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
                self.start_stream().await?;
            }
        }
    }

    fn login_to_radar(&mut self) -> Result<(), RadarError> {
        // Furuno radars use a single TCP/IP connection to send commands and
        // receive status reports, so report_addr and send_command_addr are identical.
        // Only one of these would be enough for Furuno.
        let port: u16 = match super::login_to_radar(self.session.clone(), self.info.addr) {
            Err(e) => {
                log::error!("{}: Unable to connect for login: {}", self.info.key(), e);
                return Err(RadarError::LoginFailed);
            }
            Ok(p) => p,
        };
        if port != self.info.send_command_addr.port() {
            self.info.send_command_addr.set_port(port);
            self.info.report_addr.set_port(port);
        }
        Ok(())
    }

    fn set(&mut self, control_type: &ControlType, value: f32, auto: Option<bool>) {
        match self.info.controls.set(control_type, value, auto) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} enabled {:?}",
                        self.key,
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
            .info
            .controls
            .set_value_auto(control_type, auto > 0, value)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto {}",
                        self.key,
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
            .info
            .controls
            .set_value_with_many_auto(control_type, value, auto_value)
        {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    log::debug!(
                        "{}: Control '{}' new value {} auto_value {:?} auto {:?}",
                        self.key,
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
        match self.info.controls.set_string(control, value) {
            Err(e) => {
                log::error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(v)) => {
                log::debug!("{}: Control '{}' new value '{}'", self.key, control, v);
            }
            Ok(None) => {}
        };
    }

    fn process_report(&mut self, line: &str) -> Result<(), Error> {
        let line = match line.find('$') {
            Some(pos) => {
                if pos > 0 {
                    log::warn!("{}: Ignoring first {} bytes of TCP report", self.key, pos);
                    &line[pos..]
                } else {
                    line
                }
            }
            None => {
                log::warn!("{}: TCP report dropped, no $", self.key);
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

        log::trace!("{}: processing $N{}", self.key, line);

        let mut values_iter = line.split(',');

        let cmd_str = values_iter
            .next()
            .ok_or(io::Error::new(io::ErrorKind::Other, "No command ID"))?;
        let cmd = u8::from_str_radix(cmd_str, 16)?;

        let command_id = match CommandId::from_u8(cmd) {
            Some(c) => c,
            None => {
                log::debug!("{}: ignoring unimplemented command {}", self.key, cmd_str);
                return Ok(());
            }
        };

        // Match commands that do not have just numbers as arguments first

        let strings: Vec<&str> = values_iter.collect();
        log::debug!("{}: command {:02X} strings {:?}", self.key, cmd, strings);
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
                self.set_value(&ControlType::Status, numbers[0]);
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
                let range = self.info.ranges.all.get(index).with_context(|| {
                    format!(
                        "Range index {} out of bounds for ranges {}",
                        index, self.info.ranges,
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
                self.key,
                model.to_str(),
                version
            );
            settings::update_when_model_known(&mut self.info, model, version);
            self.command_sender.set_ranges(self.info.ranges.clone());
            return;
        }
        log::error!("{}: Unknown radar type, modules {:?}", self.key, values);
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
}
