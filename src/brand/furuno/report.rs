use anyhow::{bail, Error};
use log::{debug, error, info, trace};
use num_traits::FromPrimitive;

use std::io;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::io::WriteHalf;
use tokio::net::{TcpSocket, TcpStream};
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::CommandId;
use super::CommandMode;
use super::FURUNO_RADAR_RANGES;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlUpdate};

use super::command::Command;

pub struct FurunoReportReceiver {
    info: RadarInfo,
    key: String,
    command_sender: Command,
    buf: Vec<u8>,
    stream: Option<TcpStream>,
    report_request_timeout: Instant,
    report_request_interval: Duration,
}

impl FurunoReportReceiver {
    pub fn new(info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();

        let command_sender = Command::new(&info);

        FurunoReportReceiver {
            info,
            key,
            command_sender,
            buf: Vec::with_capacity(1000),
            stream: None,

            report_request_timeout: Instant::now(),
            report_request_interval: Duration::from_millis(5000),
        }
    }

    async fn start_stream(&mut self) -> Result<(), RadarError> {
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
        debug!("{}: listening for reports", self.key);
        let mut command_rx = self.info.control_update_subscribe();

        let stream = self.stream.take().unwrap();
        let (mut read, mut write) = tokio::io::split(stream);
        self.command_sender.init(&mut write).await?;

        loop {
            self.report_request_timeout += self.report_request_interval;
            let timeout = self.report_request_timeout;

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(timeout) => {
                    self.command_sender.send(&mut write, CommandMode::New, CommandId::AliveCheck, &[]).await?;
                },

                r = read.read_buf(&mut self.buf)  => {
                    match r {
                        Ok(len) => {
                            if len > 4 {
                                if self.buf[len - 2] == 13 && self.buf[len - 1] == 10 {
                                    self.buf.truncate(len - 2);
                                    if let Err(e) = self.process_report().await {
                                        error!("{}: {}", self.key, e);
                                    }
                                }
                            }

                            self.buf.clear();
                        }
                        Err(e) => {
                            error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = command_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {
                            if let Err(e) = self.process_control_update(&mut write, cv).await {
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
                self.start_stream().await?;
            }
        }
    }

    fn set(&mut self, control_type: &ControlType, value: f32, auto: Option<bool>) {
        match self.info.controls.set(control_type, value, auto) {
            Err(e) => {
                error!("{}: {}", self.key, e.to_string());
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
                error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    debug!(
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
                error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.info.controls.get(control_type).unwrap();
                    debug!(
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

    fn set_string(&mut self, control: &ControlType, value: String) {
        match self.info.controls.set_string(control, value) {
            Err(e) => {
                error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(v)) => {
                debug!("{}: Control '{}' new value '{}'", self.key, control, v);
            }
            Ok(None) => {}
        };
    }

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        if data.len() < 4 || data[0] != '$' as u8 {
            bail!("TCP report {:?} dropped", data);
        }

        log::trace!("{}: processing {}", self.key, std::str::from_utf8(data)?);

        let args = &data[2..];
        let mut values_str = match std::str::from_utf8(args) {
            Ok(v) => v,
            Err(_) => {
                bail!(
                    "{}: Ignoring non-ASCII string from radar: {:?}",
                    self.key,
                    args
                );
            }
        }
        .split(',');

        let cmd_str = values_str
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

        let mut values = Vec::new();

        while let Some(value) = values_str.next() {
            let parsed_value = value.trim().parse::<f32>()?;
            values.push(parsed_value);
        }
        log::trace!("Parsed: ${:02X},{:?}", cmd, values);

        match command_id {
            CommandId::Status => {
                if values.len() < 1 {
                    bail!("No arguments for Status command");
                }
                self.set_value(&ControlType::Status, values[0]);
            }
            CommandId::Gain => {
                if values.len() < 5 {
                    bail!("Insufficient ({}) arguments for Gain command", values.len());
                }
                let auto = values[2] as u8;
                let gain = if auto > 0 { values[3] } else { values[1] };
                log::trace!(
                    "Gain: {} auto {} values[1]={} values[3]={}",
                    gain,
                    auto,
                    values[1],
                    values[3]
                );
                self.set_value_auto(&ControlType::Gain, gain, auto);
            }
            CommandId::Range => {
                if values.len() < 3 {
                    bail!(
                        "Insufficient ({}) arguments for Range command",
                        values.len()
                    );
                }
                if values[2] != 0. {
                    bail!("Cannot handle radar not set to NM range");
                }
                let index = values[0] as usize;
                let range = FURUNO_RADAR_RANGES.get(index).unwrap_or(&0);
                self.set_value(&ControlType::Range, *range as f32);
            }
            CommandId::OnTime => {
                let hours = values[0] / 3600.0;
                self.set_value(&ControlType::OperatingHours, hours);
            }
            CommandId::AliveCheck => {}
            _ => {
                bail!("TODO: Handle command {:?} values {:?}", command_id, values);
            }
        }
        Ok(())
    }
}
