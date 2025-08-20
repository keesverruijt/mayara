use anyhow::{bail, Error};
use enum_primitive_derive::Primitive;
use std::collections::HashMap;
use std::mem::transmute;
use std::time::Duration;
use std::{fmt, io};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::network::create_udp_multicast_listen;
use crate::radar::range::{self, Range, Ranges};
use crate::radar::{RadarError, RadarInfo, SharedRadars, Status, NAUTICAL_MILE_F64};
use crate::settings::{ControlType, ControlUpdate, ControlValue, DataUpdate};
use crate::{Cli, Session};

// use super::command::Command;
use super::command::Command;
use super::Model;

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

pub struct RaymarineReportReceiver {
    replay: bool,
    info: RadarInfo,
    key: String,
    report_buf: Vec<u8>,
    report_socket: Option<UdpSocket>,
    radars: SharedRadars,
    model: Model,
    command_sender: Option<Command>,
    data_update_tx: broadcast::Sender<DataUpdate>,
    control_update_rx: broadcast::Receiver<ControlUpdate>,
    info_request_timeout: Instant,
    report_request_timeout: Instant,
    reported_unknown: HashMap<u32, bool>,
}

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct RMRadarReport {
    _id: [u8; 4],            // 0x010001  // 0-3
    ranges: [u8; 4 * 11],    // 4 - 47
    _fieldx_1: [u8; 4 * 33], // 48 -

    status: u8, // 2 - warmup, 1 - transmit, 0 - standby, 6 - shutting down (warmup time - countdown), 3 - shutdown  // 180
    _fieldx_2: [u8; 3], // 181
    warmup_time: u8, // 184
    signal_strength: u8, // number of bars   // 185

    _fieldx_3: [u8; 7], // 186
    range_id: u8,       // 193
    _fieldx_4: [u8; 2], // 194
    auto_gain: u8,      // 196
    _fieldx_5: [u8; 3], // 197
    gain: [u8; 4],      // 200
    auto_sea: u8,       // 0 - disabled; 1 - harbour, 2 - offshore, 3 - coastal   // 204
    _fieldx_6: [u8; 3], // 205
    sea_value: u8,      // 208
    rain_enabled: u8,   // 209
    _fieldx_7: [u8; 3], // 210
    rain_value: u8,     // 213
    ftc_enabled: u8,    // 214
    _fieldx_8: [u8; 3], // 215
    ftc_value: u8,      // 218
    auto_tune: u8,
    _fieldx_9: [u8; 3],
    tune: u8,
    bearing_offset: [u8; 2], // degrees * 10; left - negative, right - positive
    interference_rejection: u8,
    _fieldx_10: [u8; 3],
    target_expansion: u8,
    _fieldx_11: [u8; 13],
    mbs_enabled: u8, // Main Bang Suppression enabled if 1
}

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct QuantumControls {
    gain_auto: u8,       // @ 0
    gain: u8,            // @ 1
    color_gain_auto: u8, // @ 2
    color_gain: u8,      // @ 3
    sea_auto: u8,        // @ 4
    sea: u8,             // @ 5
    rain_auto: u8,       // @ 6
    rain: u8,            // @ 7
}

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct QuantumRadarReport {
    _id: [u8; 4],                   // @0 0x280002
    status: u8,                     // @4 0 - standby ; 1 - transmitting
    _something_1: [u8; 9],          // @5
    bearing_offset: [u8; 2],        // @14
    _something_2: u8,               // @16
    interference_rejection: u8,     // @17
    _something_3: [u8; 2],          // @18
    range_index: u8,                // @20
    mode: u8,                       // @21 harbor - 0, coastal - 1, offshore - 2, weather - 3
    controls: [QuantumControls; 4], // @22 controls indexed by mode
    target_expansion: u8,           // @54
    _something_9: u8,               // @55
    _something_10: [u8; 3],         // @56
    mbs_enabled: u8,                // @59
    _something_11: [u8; 88],        // @60
    ranges: [u8; 20 * 4],           // @148
    _something_12: [u8; 32],        // @228
}

impl QuantumRadarReport {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 260] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

impl RaymarineReportReceiver {
    pub fn new(
        session: Session,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        model: Model,
    ) -> RaymarineReportReceiver {
        let key = info.key();

        let args = session.read().unwrap().args.clone();
        let replay = args.replay;
        log::debug!(
            "{}: Creating NavicoReportReceiver with args {:?}",
            key,
            args
        );
        // If we are in replay mode, we don't need a command sender, as we will not send any commands
        let command_sender = if !replay {
            log::debug!("{}: Starting command sender", key);
            Some(Command::new(session.clone(), info.clone(), model.clone()))
        } else {
            log::debug!("{}: No command sender, replay mode", key);
            None
        };

        let control_update_rx = info.controls.control_update_subscribe();
        let data_update_tx = info.controls.get_data_update_tx();

        let now = Instant::now();
        RaymarineReportReceiver {
            replay,
            key,
            info,
            report_buf: Vec::with_capacity(1000),
            report_socket: None,
            radars,
            model,
            command_sender,
            info_request_timeout: now,
            report_request_timeout: now,
            data_update_tx,
            control_update_rx,
            reported_unknown: HashMap::new(),
        }
    }

    async fn start_report_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(&self.info.report_addr, &self.info.nic_addr) {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.key,
                    &self.info.report_addr,
                    &self.info.nic_addr,
                    e
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
        log::debug!("{}: listening for reports", self.key);

        loop {
            let timeout = self.report_request_timeout;
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(timeout) => {
                     self.send_report_requests().await?;

                },

                r = self.report_socket.as_ref().unwrap().recv_buf_from(&mut self.report_buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_report().await {
                                log::error!("{}: {}", self.key, e);
                            }
                            self.report_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },
                r = self.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {let _ = self.process_control_update(cv).await;},
                    }
                }
            }
        }
    }

    async fn process_control_update(
        &mut self,
        control_update: ControlUpdate,
    ) -> Result<(), RadarError> {
        let cv = control_update.control_value;
        let reply_tx = control_update.reply_tx;

        if let Some(command_sender) = &mut self.command_sender {
            if let Err(e) = command_sender.set_control(&cv, &self.info.controls).await {
                return self
                    .info
                    .controls
                    .send_error_to_client(reply_tx, &cv, &e)
                    .await;
            } else {
                self.info.controls.set_refresh(&cv.id);
            }
        }

        Ok(())
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        // self.command_sender.send_report_requests().await?;
        self.report_request_timeout += REPORT_REQUEST_INTERVAL;
        Ok(())
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_report_socket().await?;
        loop {
            if self.report_socket.is_some() {
                match self.socket_loop(&subsys).await {
                    Err(RadarError::Shutdown) => {
                        return Ok(());
                    }
                    _ => {
                        // Ignore, reopen socket
                    }
                }
                self.report_socket = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_report_socket().await?;
            }
        }
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
                        "{}: Control '{}' new value {} auto {:?} enabled {:?}",
                        self.key,
                        control_type,
                        control.value(),
                        control.auto,
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
        self.set(control_type, value, Some(auto > 0))
    }

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

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

        if data.len() < 4 {
            bail!("UDP report len {} dropped", data.len());
        }
        log::debug!("{}: UDP report {:02X?}", self.key, data);

        let id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        if id == 0x280002 {
            let report = QuantumRadarReport::transmute(data)?;
            log::debug!("{}: Quantum report {:?}", self.key, report);
            self.process_quantum_report(report).await?;
        } else if self.reported_unknown.get(&id).is_none() {
            log::warn!("{}: Unknown report ID {:08X?}", self.key, id);
            self.reported_unknown.insert(id, true);
        }

        Ok(())
    }

    async fn process_quantum_report(&mut self, report: QuantumRadarReport) -> Result<(), Error> {
        // Process the Quantum radar report
        log::debug!("{}: Quantum report {:?}", self.key, report);

        // Update controls based on the report
        let status = match report.status {
            0x00 => Status::Standby,
            0x01 => Status::Transmit,
            0x02 => Status::SpinningUp,
            0x03 => Status::Off,
            _ => {
                log::warn!("{}: Unknown status {}", self.key, report.status);
                Status::Standby // Default to Standby if unknown
            }
        };
        self.set_value(&ControlType::Status, status as i32 as f32);

        if self.info.ranges.is_empty() {
            let mut ranges = Ranges::empty();

            for i in (0..report.ranges.len()).step_by(4) {
                let range_bytes = &report.ranges[i..i + 4];
                let range = u32::from_le_bytes(range_bytes.try_into().unwrap());
                let meters = (range as f64 * 1.852f64) as i32; // Convert to nautical miles

                ranges.push(Range::new(meters, i));
            }
            self.info.ranges = Ranges::new(ranges.all);
            log::info!("{}: Ranges initialized: {}", self.key, self.info.ranges);
        }
        self.set_value(
            &ControlType::Range,
            self.info.ranges.get_distance(report.range_index as usize) as f32,
        );

        let mode = report.mode as usize;
        if mode <= 3 {
            self.set_value(&ControlType::Mode, mode as f32);
            self.set_value_auto(
                &ControlType::Gain,
                report.controls[mode].gain as f32,
                report.controls[mode].gain_auto,
            );
            self.set_value_auto(
                &ControlType::ColorGain,
                report.controls[mode].color_gain as f32,
                report.controls[mode].color_gain_auto,
            );
            self.set_value_auto(
                &ControlType::SeaState,
                report.controls[mode].sea as f32,
                report.controls[mode].sea_auto,
            );
            self.set_value_auto(
                &ControlType::Rain,
                report.controls[mode].rain as f32,
                report.controls[mode].rain_auto,
            );
        } else {
            log::warn!("{}: Unknown mode {}", self.key, report.mode);
        }
        self.set_value(
            &ControlType::TargetExpansion,
            report.target_expansion as f32,
        );
        self.set_value(
            &ControlType::InterferenceRejection,
            report.interference_rejection as f32,
        );
        self.set_value(
            &ControlType::BearingAlignment,
            i16::from_le_bytes(report.bearing_offset) as f32,
        );
        self.set_value(&ControlType::MainBangSuppression, report.mbs_enabled as f32);

        Ok(())
    }
}
