use anyhow::{bail, Error};
use enum_primitive_derive::Primitive;
use log::{debug, info, trace};
use std::io;
use std::mem::transmute;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_shutdown::Shutdown;

use crate::radar::RadarLocationInfo;
use crate::util::{c_wide_string, create_multicast};

use super::command::{self, Command};
use super::{NavicoSettings, NavicoType};

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

#[derive(Primitive)]
enum RawSubtype {
    ReportBr24 = 0x0f,
    Report3g = 0x08,
    Report4G = 0x01,
    ReportHalo = 0x00,
}

impl From<Option<RawSubtype>> for NavicoType {
    fn from(v: Option<RawSubtype>) -> Self {
        match v {
            None => NavicoType::Unknown,
            Some(RawSubtype::ReportBr24) => NavicoType::BR24,
            Some(RawSubtype::Report3g) => NavicoType::Navico3g,
            Some(RawSubtype::Report4G) => NavicoType::Navico4g,
            Some(RawSubtype::ReportHalo) => NavicoType::HALO,
        }
    }
}

#[repr(packed)]
struct RadarReport3_129 {
    _what: u8,
    _command: u8,
    subtype: u8,             // So far: 01 = 4G and new 3G, 08 = 3G, 0F = BR24, 00 = HALO
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
                Ok(())
            }
        }
    }

    async fn socket_loop(&mut self, shutdown: &Shutdown) -> io::Result<()> {
        loop {
            let shutdown_handle = shutdown.handle();
            tokio::select! { biased;
              _ = shutdown_handle => {
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
        Ok(())
    }

    pub async fn run(&mut self, shutdown: Shutdown) -> io::Result<()> {
        self.start_socket().await.unwrap();
        loop {
            if self.sock.is_some() {
                self.socket_loop(&shutdown).await.unwrap();
                self.sock = None;
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_socket().await.unwrap();
            }
        }
    }

    fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        trace!(
            "{} via {}: report received: {:02X?}",
            self.info.report_addr,
            self.info.nic_addr,
            data
        );

        if data.len() < 2 {
            bail!("UDP report len {} dropped", data.len());
        }

        if data[1] != 0xC4 {
            bail!("Unknown report {:02X?} dropped", data);
        }
        let report_identification = data[0];
        match report_identification {
            REPORT_03_C4_129 => {
                let report = RadarReport3_129::transmute(data)?;

                let subtype = report.subtype;
                let hours = u32::from_le_bytes(report.hours);
                let firmware_date = c_wide_string(&report.firmware_date);
                let firmware_time = c_wide_string(&report.firmware_time);

                info!(
                    "subtype={} hours={} firmware: {:?} {:?}",
                    subtype, hours, firmware_date, firmware_time
                );

                let raw_subtype: Result<RawSubtype, _> = subtype.try_into();
                if raw_subtype.is_ok() {
                    let subtype = raw_subtype.ok().into();
                    if self.settings.subtype.load() != subtype {
                        info!("Radar is type {}", subtype);
                        self.settings.subtype.store(subtype);

                        let mut radars = self.settings.radars.write().unwrap();
                        if let Some(info) = radars.info.get_mut(&self.key) {
                            info.model = Some(format!("{}", subtype));
                        }
                    }
                    self.subtype_repeat = Duration::from_secs(600);
                }
            }
            _ => {
                bail!(
                    "Unknown report identification {} {:02X?} dropped",
                    report_identification,
                    data
                );
            }
        };

        Ok(())
    }
}
