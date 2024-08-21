use anyhow::{bail, Error};
use enum_primitive_derive::Primitive;
use log::{debug, info, trace};
use std::mem::transmute;
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};
use tokio::net::UdpSocket;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::{DopplerMode, RadarLocationInfo};
use crate::util::{c_wide_string, create_multicast};

use super::command::{self, Command};
use super::{Model, NavicoSettings};

pub struct Receive {
    info: RadarLocationInfo,
    key: String,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
    settings: Arc<NavicoSettings>,
    command_sender: Command,
    subtype_timeout: Instant,
    subtype_repeat: Duration,
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
    _what: u8,                        // 0  0x08
    _command: u8,                     // 1  0xC4
    sea_state: u8,                    // 2
    local_interference_rejection: u8, // 3
    scan_speed: u8,                   // 4
    sls_auto: u8,                     // 5 installation: sidelobe suppression auto
    _field6: u8,                      // 6
    _field7: u8,                      // 7
    _field8: u8,                      // 8
    side_lobe_suppression: u8,        // 9 installation: sidelobe suppression
    _field10: u16,                    // 10-11
    noise_rejection: u8,              // 12    noise rejection
    target_sep: u8,                   // 13
    sea_clutter: u8,                  // 14 sea clutter on Halo
    auto_sea_clutter: i8,             // 15 auto sea clutter on Halo
    _field13: u8,                     // 16
    _field14: u8,                     // 17
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
            let report: [u8; 18] = bytes.try_into()?;
            transmute(report)
        })
    }
}

impl RadarReport8_21 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 21] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_08_C4_18_OR_21: u8 = 0x08;

impl Receive {
    pub fn new(
        info: RadarLocationInfo,
        settings: Arc<NavicoSettings>,
        command_sender: Command,
    ) -> Receive {
        let key = info.key();
        Receive {
            key: key,
            info: info,
            buf: Vec::with_capacity(1024),
            sock: None,
            settings,
            command_sender,
            subtype_timeout: Instant::now(),
            subtype_repeat: Duration::from_millis(5000),
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_multicast(&self.info.report_addr, &self.info.nic_addr) {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "{} via {}: listening for reports",
                    &self.info.report_addr, &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!(
                    "{} via {}: create multicast failed: {}",
                    &self.info.report_addr, &self.info.nic_addr, e
                );
                Err(e)
            }
        }
    }

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> io::Result<()> {
        loop {
            tokio::select! { biased;
              _ = subsys.on_shutdown_requested() => {
                    break;
              },
              _ = sleep_until(self.subtype_timeout) => {
                    self.send_report_requests().await?;
                    self.subtype_timeout += self.subtype_repeat;
              },
              _ = self.sock.as_ref().unwrap().recv_buf_from(&mut self.buf)  => {
                  let _ = self.process_report();
                  self.buf.clear();
              },
            };
        }
        Ok(())
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

    pub async fn run(mut self, subsys: SubsystemHandle) -> io::Result<()> {
        self.start_socket().await?;
        loop {
            if self.sock.is_some() {
                let _ = self.socket_loop(&subsys).await; // Ignore the error, re-open socket
                self.sock = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_socket().await?;
            }
        }
    }

    fn process_report(&mut self) -> Result<(), Error> {
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
                return self.process_report_01();
            }
            REPORT_03_C4_129 => {
                return self.process_report_03();
            }
            REPORT_08_C4_18_OR_21 => {
                return self.process_report_08();
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

    fn process_report_01(&mut self) -> Result<(), Error> {
        let report = RadarReport1_18::transmute(&self.buf)?;

        let status: Result<Status, _> = report.status.try_into();
        if status.is_err() {
            bail!("{}: Unknown radar status {}", self.key, report.status);
        }
        debug!("{}: status {}", self.key, status.unwrap());
        Ok(())
    }

    fn process_report_03(&mut self) -> Result<(), Error> {
        let report = RadarReport3_129::transmute(&self.buf)?;
        let model = report.model;
        let hours = u32::from_le_bytes(report.hours);
        let firmware_date = c_wide_string(&report.firmware_date);
        let firmware_time = c_wide_string(&report.firmware_time);
        info!(
            "{}: model={} hours={} firmware: {:?} {:?}",
            self.key, model, hours, firmware_date, firmware_time
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

                    let mut radars = self.settings.radars.write().unwrap();
                    if let Some(info) = radars.info.get_mut(&self.key) {
                        info.model = Some(format!("{}", model));
                    }
                }
            }
        }

        self.subtype_repeat = Duration::from_secs(600);

        Ok(())
    }

    fn process_report_08(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        if data.len() != 18 && data.len() != 21 {
            bail!("{}: Report 0x08C4 invalid length {}", self.key, data.len());
        }

        let report = RadarReport8_18::transmute(&data[0..18])?;

        let sea_state = report.sea_state;
        let local_interference_rejection = report.local_interference_rejection;
        let scan_speed = report.scan_speed;
        let sidelobe_suppression_auto = report.sls_auto;
        let sidelobe_suppression = report.side_lobe_suppression;
        let noise_reduction = report.noise_rejection;
        let target_sep = report.target_sep;
        let sea_clutter = report.sea_clutter;
        let auto_sea_clutter = report.auto_sea_clutter;

        debug!("{}: sea_state={} local_interference_rej={} scan_speed={} \
                sidelobe_suppression={} (auto {}) noise_reduction={} target_sep={} sea_clutter={} (auto {})",
                self.key, sea_state, local_interference_rejection, scan_speed,
                sidelobe_suppression, sidelobe_suppression_auto, noise_reduction, target_sep, sea_clutter, auto_sea_clutter);

        if data.len() == 21 {
            let report = RadarReport8_21::transmute(&data[0..21])?;

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
                        "{}: doppler state={} speed={}",
                        self.key, doppler_mode, doppler_speed
                    );
                }
            }
        }
        Ok(())
    }
}
