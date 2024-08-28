use anyhow::{bail, Error};
use enum_primitive_derive::Primitive;
use log::{debug, error, info, trace};
use std::mem::transmute;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::{DopplerMode, Legend, RadarError, RadarInfo};
use crate::settings::ControlType;
use crate::util::{c_wide_string, create_multicast};

use super::command::{self, Command};
use super::settings::NavicoControls;
use super::{DataUpdate, Model, NavicoSettings};

pub struct NavicoReportReceiver {
    info: RadarInfo,
    key: String,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
    settings: Arc<NavicoSettings>,
    command_sender: Command,
    data_tx: Sender<DataUpdate>,
    subtype_timeout: Instant,
    subtype_repeat: Duration,
    controls: Option<NavicoControls>,
}

#[derive(Primitive, Debug)]
enum Status {
    Standby = 0x01,
    Transmit = 0x02,
    SpinningUp = 0x05,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

#[repr(packed)]
struct RadarReport1_18 {
    _what: u8,
    _command: u8,
    status: u8,
    _u00: [u8; 15], // Lots of unknown
}

impl RadarReport1_18 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 18] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_01_C4_18: u8 = 0x01;

#[repr(packed)]
struct RadarReport2_99 {
    _what: u8,
    _command: u8,
    range: [u8; 4],             // 2..6 = range
    _u00: [u8; 1],              // 6
    mode: u8, // 7 = mode, 0 = custom, 1 = harbor, 2 = offshore, 3 = ?, 4 = bird, 5 = weather
    _u01: [u8; 4], // 8..12
    gain: u8, // 12
    sea_auto: u8, // 13 = sea_auto, 0 = off, 1 = harbor, 2 = offshore
    _u02: [u8; 3], // 14..17
    sea: [u8; 4], // 17..21
    _u03: u8, // 21
    rain: u8, // 22
    _u04: [u8; 11], // 23..34
    interference_rejection: u8, // 34
    _u05: [u8; 3], // 35..38
    target_expansion: u8, // 38
    _u06: [u8; 3], // 39..42
    target_boost: u8, // 42
    _u07: [u8; 56], // 43..99
}

impl RadarReport2_99 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 99] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_02_C4_99: u8 = 0x02;

#[repr(packed)]
struct RadarReport3_129 {
    _what: u8,
    _command: u8,
    model: u8,               // So far: 01 = 4G and new 3G, 08 = 3G, 0F = BR24, 00 = HALO
    _u00: [u8; 31],          // Lots of unknown
    hours: [u8; 4],          // Hours of operation
    _u01: [u8; 20],          // Lots of unknown
    firmware_date: [u8; 32], // Wide chars, assumed UTF16
    firmware_time: [u8; 32], // Wide chars, assumed UTF16
    _u02: [u8; 7],
}

impl RadarReport3_129 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 129] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_03_C4_129: u8 = 0x03;

#[repr(packed)]
struct RadarReport8_18 {
    // 08 c4  length 18
    _what: u8,                  // 0  0x08
    _command: u8,               // 1  0xC4
    sea_state: u8,              // 2
    interference_rejection: u8, // 3
    scan_speed: u8,             // 4
    sls_auto: u8,               // 5 installation: sidelobe suppression auto
    _field6: u8,                // 6
    _field7: u8,                // 7
    _field8: u8,                // 8
    side_lobe_suppression: u8,  // 9 installation: sidelobe suppression
    _field10: u16,              // 10-11
    noise_rejection: u8,        // 12    noise rejection
    target_sep: u8,             // 13
    sea_clutter: u8,            // 14 sea clutter on Halo
    auto_sea_clutter: i8,       // 15 auto sea clutter on Halo
    _field13: u8,               // 16
    _field14: u8,               // 17
}

struct RadarReport8_21 {
    _old: RadarReport8_18,
    doppler_state: u8,
    doppler_speed: [u8; 2], // doppler speed threshold in values 0..1594 (in cm/s).
}

impl RadarReport8_18 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; size_of::<RadarReport8_18>()] = bytes.try_into()?;
            transmute(report)
        })
    }
}

impl RadarReport8_21 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; size_of::<RadarReport8_21>()] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_08_C4_18_OR_21: u8 = 0x08;

impl NavicoReportReceiver {
    pub fn new(
        info: RadarInfo,
        settings: Arc<NavicoSettings>,
        command_sender: Command,
        data_tx: Sender<DataUpdate>,
    ) -> NavicoReportReceiver {
        let key = info.key();
        NavicoReportReceiver {
            key: key,
            info: info,
            buf: Vec::with_capacity(500),
            sock: None,
            settings,
            command_sender,
            subtype_timeout: Instant::now(),
            subtype_repeat: Duration::from_millis(5000),
            data_tx,
            controls: None,
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_multicast(&self.info.report_addr, &self.info.nic_addr) {
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
                Err(e)
            }
        }
    }

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        debug!("{}: listening for reports", self.key);
        loop {
            tokio::select! { biased;
              _ = subsys.on_shutdown_requested() => {
                    info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
              },
              _ = sleep_until(self.subtype_timeout) => {
                    self.send_report_requests().await?;
                    self.subtype_timeout += self.subtype_repeat;
              },
              r = self.sock.as_ref().unwrap().recv_buf_from(&mut self.buf)  => {
                    match r {
                        Ok((len, addr)) => {
                            debug!("{}: received {} bytes from {}", self.key, len, addr);
                            let _ = self.process_report().await;
                            self.buf.clear();
                        }
                        Err(e) => {
                            error!("{}: receive error: {}", self.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
              },
            };
        }
    }

    async fn send_report_requests(&mut self) -> Result<(), io::Error> {
        self.command_sender
            .send(&command::REQUEST_03_REPORT)
            .await?;
        self.command_sender
            .send(&command::REQUEST_MANY2_REPORT)
            .await?;
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

    fn set(&mut self, control_type: &ControlType, value: i32) {
        if let Some(controls) = self.controls.as_mut() {
            match controls.set(control_type, value) {
                Err(e) => {
                    error!("{}: {}", self.key, e.to_string());
                }
                Ok(Some(())) => {
                    if log::log_enabled!(log::Level::Debug) {
                        let control = controls.controls.get(control_type).unwrap();
                        debug!(
                            "{}: Control '{}' new value {}",
                            self.key,
                            control_type,
                            control.value_string()
                        );
                    }
                }
                Ok(None) => {}
            }
        };
    }

    fn set_auto(&mut self, control_type: &ControlType, value: i32, auto: u8) {
        if let Some(controls) = self.controls.as_mut() {
            match controls.set_auto(control_type, auto > 0, value) {
                Err(e) => {
                    error!("{}: {}", self.key, e.to_string());
                }
                Ok(Some(())) => {
                    if log::log_enabled!(log::Level::Debug) {
                        let control = controls.controls.get(control_type).unwrap();
                        debug!(
                            "{}: Control '{}' new value {} auto {}",
                            self.key,
                            control_type,
                            control.value_string(),
                            auto
                        );
                    }
                }
                Ok(None) => {}
            }
        };
    }

    fn set_string(&mut self, control: &ControlType, value: String) {
        if let Some(controls) = self.controls.as_mut() {
            match controls.set_string(control, value) {
                Err(e) => {
                    error!("{}: {}", self.key, e.to_string());
                }
                Ok(Some(v)) => {
                    debug!("{}: Control '{}' new value '{}'", self.key, control, v);
                }
                Ok(None) => {}
            }
        };
    }

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        trace!("{}: report received: {:02X?}", self.key, data);

        if data.len() < 2 {
            bail!("UDP report len {} dropped", data.len());
        }

        if data[1] != 0xC4 {
            bail!("Unknown report {:02X?} dropped", data);
        }
        let report_identification = data[0];
        match report_identification {
            REPORT_01_C4_18 => {
                return self.process_report_01().await;
            }
            REPORT_02_C4_99 => {
                return self.process_report_02().await;
            }
            REPORT_03_C4_129 => {
                return self.process_report_03().await;
            }
            REPORT_08_C4_18_OR_21 => {
                return self.process_report_08().await;
            }
            _ => {
                bail!(
                    "Unknown report identification {} {:02X?} dropped",
                    report_identification,
                    data
                );
            }
        };
    }

    async fn process_report_01(&mut self) -> Result<(), Error> {
        let report = RadarReport1_18::transmute(&self.buf)?;

        let status: Result<Status, _> = report.status.try_into();
        if status.is_err() {
            bail!("{}: Unknown radar status {}", self.key, report.status);
        }
        self.set(&ControlType::Status, report.status as i32);

        Ok(())
    }

    async fn process_report_02(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        if data.len() != size_of::<RadarReport2_99>() {
            bail!("{}: Report 0x02C4 invalid length {}", self.key, data.len());
        }

        let report = RadarReport2_99::transmute(&data)?;

        let mode = report.mode as i32;
        let range = i32::from_le_bytes(report.range);
        let gain = report.gain as i32;
        let sea_auto = report.sea_auto;
        let sea = i32::from_le_bytes(report.sea);
        let rain = report.rain as i32;
        let interference_rejection = report.interference_rejection as i32;
        let target_expansion = report.target_expansion as i32;
        let target_boost = report.target_boost as i32;

        debug!("{}: range={} mode={} gain={} sea={} sea_auto={} rain={} interference_rejection={} target_expansion={} \
                target_boost={}",
                self.key, range, mode, gain, sea, sea_auto, rain, interference_rejection, target_expansion, target_boost);

        self.set(&ControlType::Range, range);
        self.set(&ControlType::Mode, mode);
        self.set(&ControlType::Gain, gain);
        self.set_auto(&ControlType::Sea, sea, sea_auto);
        self.set(&ControlType::Rain, rain);
        self.set(&ControlType::InterferenceRejection, interference_rejection);
        self.set(&ControlType::TargetExpansion, target_expansion);
        self.set(&ControlType::TargetBoost, target_boost);

        Ok(())
    }

    async fn process_report_03(&mut self) -> Result<(), Error> {
        let report = RadarReport3_129::transmute(&self.buf)?;
        let model = report.model;
        let hours = i32::from_le_bytes(report.hours);
        let firmware_date = c_wide_string(&report.firmware_date);
        let firmware_time = c_wide_string(&report.firmware_time);
        trace!(
            "{}: model={} hours={} firmware: {:?} {:?}",
            self.key,
            model,
            hours,
            firmware_date,
            firmware_time
        );
        let model: Result<Model, _> = model.try_into();
        match model {
            Err(_) => {
                bail!("{}: Unknown model # {}", self.key, report.model);
            }
            Ok(model) => {
                if self.settings.model.load() != model {
                    info!("{}: Radar is model {}", self.key, model);
                    self.settings.model.store(model);

                    match self.generate_legend(model) {
                        Some(legend) => {
                            self.data_tx.send(DataUpdate::Legend(legend)).await?;
                        }
                        None => {
                            error!("{}: Weird, no legend now", self.key);
                        }
                    }
                    self.controls = Some(NavicoControls::new2(
                        model,
                        self.info.radar_message_tx.clone(),
                    ));
                }
            }
        }

        let firmware = format!("{} {}", firmware_date, firmware_time);
        self.set(&ControlType::OperatingHours, hours);
        self.set_string(&ControlType::FirmwareVersion, firmware);
        self.subtype_repeat = Duration::from_secs(600);

        Ok(())
    }

    ///
    /// Generate a legend to all byte values stored in the spokes.
    ///
    /// Separate function so we don't lock radars longer than necessary
    /// Once we return the lock is gone, and the legend can be sent to
    /// the data thread.
    ///
    fn generate_legend(&mut self, model: Model) -> Option<Legend> {
        match self.settings.radars.write() {
            Ok(mut radars) => {
                if let Some(info) = radars.info.get_mut(&self.key) {
                    info.model = Some(model.to_string());
                    info.set_legend(model == Model::HALO);
                    if let Some(legend) = &info.legend {
                        return Some(legend.clone());
                    }
                }
                None
            }
            Err(_) => {
                panic!("Poisoned RwLock on Radars");
            }
        }
    }

    async fn process_report_08(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        if data.len() != size_of::<RadarReport8_18>() && data.len() != size_of::<RadarReport8_21>()
        {
            bail!("{}: Report 0x08C4 invalid length {}", self.key, data.len());
        }

        let report = RadarReport8_18::transmute(&data[0..size_of::<RadarReport8_18>()])?;

        let sea_state = report.sea_state;
        let interference_rejection = report.interference_rejection;
        let scan_speed = report.scan_speed;
        let sidelobe_suppression_auto = report.sls_auto;
        let sidelobe_suppression = report.side_lobe_suppression;
        let noise_reduction = report.noise_rejection;
        let target_sep = report.target_sep;
        let sea_clutter = report.sea_clutter;
        let auto_sea_clutter = report.auto_sea_clutter;

        debug!("{}: sea_state={} interference_rej={} scan_speed={} \
                sidelobe_suppression={} (auto {}) noise_reduction={} target_sep={} sea_clutter={} (auto {})",
                self.key, sea_state, interference_rejection, scan_speed,
                sidelobe_suppression, sidelobe_suppression_auto, noise_reduction, target_sep, sea_clutter, auto_sea_clutter);

        if data.len() == size_of::<RadarReport8_21>() {
            let report = RadarReport8_21::transmute(&data)?;

            let doppler_speed = u16::from_le_bytes(report.doppler_speed);
            let doppler_state = report.doppler_state;

            let doppler_mode: Result<DopplerMode, _> = doppler_state.try_into();
            match doppler_mode {
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.key,
                        report.doppler_state
                    );
                }
                Ok(doppler_mode) => {
                    debug!(
                        "{}: doppler mode={} speed={}",
                        self.key, doppler_mode, doppler_speed
                    );
                    self.data_tx.send(DataUpdate::Doppler(doppler_mode)).await?;
                }
            }
        }

        self.set(&ControlType::SeaState, sea_state as i32);
        self.set(
            &ControlType::InterferenceRejection,
            interference_rejection as i32,
        );
        self.set(&ControlType::ScanSpeed, scan_speed as i32);

        Ok(())
    }
}
