use anyhow::{bail, Error};
use log::{debug, error, info, trace};
use std::io;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::network::create_udp_multicast_listen;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlUpdate};

pub struct FurunoReportReceiver {
    info: RadarInfo,
    key: String,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
    report_request_timeout: Instant,
    report_request_interval: Duration,
}

impl FurunoReportReceiver {
    pub fn new(info: RadarInfo) -> FurunoReportReceiver {
        let key = info.key();

        FurunoReportReceiver {
            key: key,
            info: info,
            buf: Vec::with_capacity(1000),
            sock: None,

            report_request_timeout: Instant::now(),
            report_request_interval: Duration::from_millis(5000),
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(&self.info.report_addr, &self.info.nic_addr) {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "{}: {} via {}: listening for reports",
                    self.key, &self.info.report_addr, &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.key, &self.info.report_addr, &self.info.nic_addr, e
                );
                Ok(())
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        debug!("{}: listening for reports", self.key);
        let mut command_rx = self.info.control_update_subscribe();

        let report_socket = self.sock.take().unwrap();

        loop {
            self.report_request_timeout += self.report_request_interval;
            let timeout = self.report_request_timeout;
            self.set_value(&ControlType::Range, 400.);
            self.set_value_auto(&ControlType::Gain, 50., 1);

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(timeout) => {
                },

                r = report_socket.recv_buf_from(&mut self.buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_report().await {
                                error!("{}: {}", self.key, e);
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
                    let _ = self.process_control_update(r).await;
                }
            }
        }
    }

    async fn process_control_update(
        &mut self,
        r: Result<ControlUpdate, tokio::sync::broadcast::error::RecvError>,
    ) -> Result<(), RadarError> {
        if r.is_err() {
            return Err(RadarError::Shutdown);
        };
        let control_update = r.unwrap();

        #[cfg(todo)]
        match control_message {
            ControlUpdate::Value(reply_tx, cv) => {
                if let Err(e) = self
                    .command_sender
                    .set_control(&cv, &self.info.controls)
                    .await
                {
                    return self
                        .info
                        .controls
                        .send_error_to_controller(&reply_tx, &cv, e)
                        .await;
                } else {
                    self.info.controls.set_refresh(&cv.id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_socket().await?;
        loop {
            if self.sock.is_some() {
                match self.socket_loop(&subsys).await {
                    Err(RadarError::Shutdown) => {
                        return Ok(());
                    }
                    _ => {
                        // Ignore, reopen socket
                    }
                }
                self.sock = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_socket().await?;
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

        if data.len() < 2 {
            bail!("UDP report len {} dropped", data.len());
        }

        if data[1] != 0xc4 {
            if data[1] == 0xc6 {
                match data[0] {
                    0x11 => {
                        if data.len() != 3 || data[2] != 0 {
                            bail!("Strange content of report 0x0a 0xc6: {:02X?}", data);
                        }
                        // this is just a response to the MFD sending 0x0a 0xc2,
                        // not sure what purpose it serves.
                    }
                    _ => {
                        trace!("Unknown report 0x{:02x} 0xc6: {:02X?}", data[0], data);
                    }
                }
            } else {
                trace!("Unknown report {:02X?} dropped", data)
            }
            return Ok(());
        }

        Ok(())
    }
}
