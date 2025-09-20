use anyhow::{bail, Error};
use bincode::deserialize;
use serde::Deserialize;
use std::cmp::min;
use std::io;
use std::mem::transmute;
use std::net::SocketAddr;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::time::{sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use super::command::Command;
use super::Model;
use super::{NAVICO_SPOKES, NAVICO_SPOKES_RAW, RADAR_LINE_DATA_LENGTH, SPOKES_PER_FRAME};

use crate::brand::navico::info::{
    HaloHeadingPacket, HaloNavigationPacket, HaloSpeedPacket, Information,
};
use crate::brand::navico::{NAVICO_INFO_ADDRESS, NAVICO_SPEED_ADDRESS_A, NAVICO_SPOKE_LEN};
use crate::locator::LocatorId;
use crate::network::create_udp_multicast_listen;
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::range::{RangeDetection, RangeDetectionResult};
use crate::radar::spoke::{to_protobuf_spoke, GenericSpoke};
use crate::radar::target::MS_TO_KN;
use crate::radar::trail::TrailBuffer;
use crate::radar::{
    CommandSender, CommonRadar, DopplerMode, Legend, RadarError, RadarInfo, SharedRadars,
    SpokeBearing, Status, BYTE_LOOKUP_LENGTH,
};
use crate::settings::{ControlType, ControlValue};
use crate::util::PrintableSpoke;
use crate::util::{c_string, c_wide_string};
use crate::Session;

/*
 Heading on radar. Observed in field:
 - Hakan: BR24, no RI: 0x9234 = negative, with recognisable 1234 in hex?
 - Marcus: 3G, RI, true heading: 0x45be
 - Kees: 4G, RI, mag heading: 0x07d6 = 2006 = 176,6 deg
 - Kees: 4G, RI, no heading: 0x8000 = -1 = negative
 - Kees: Halo, true heading: 0x4xxx => true
 Known values for heading value:
*/
const HEADING_TRUE_FLAG: u16 = 0x4000;
const HEADING_MASK: u16 = NAVICO_SPOKES_RAW - 1;
fn is_heading_true(x: u16) -> bool {
    (x & HEADING_TRUE_FLAG) != 0
}
fn is_valid_heading_value(x: u16) -> bool {
    (x & !(HEADING_TRUE_FLAG | HEADING_MASK)) == 0
}
fn extract_heading_value(x: u16) -> Option<u16> {
    match is_valid_heading_value(x) && is_heading_true(x) {
        true => Some(x & HEADING_MASK),
        false => None,
    }
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct Br24Header {
    header_len: u8,        // 1 bytes
    status: u8,            // 1 bytes
    _scan_number: [u8; 2], // 1 byte (HALO and newer), 2 bytes (4G and older)
    _mark: [u8; 4],        // 4 bytes, on BR24 this is always 0x00, 0x44, 0x0d, 0x0e
    angle: [u8; 2],        // 2 bytes
    heading: [u8; 2],      // 2 bytes heading with RI-10/11. See bitmask explanation above.
    range: [u8; 4],        // 4 bytes
    _u01: [u8; 2],         // 2 bytes blank
    _u02: [u8; 2],         // 2 bytes
    _u03: [u8; 4],         // 4 bytes blank
} /* total size = 24 */

#[derive(Deserialize, Debug, Clone, Copy)]
#[repr(packed)]
struct Br4gHeader {
    header_len: u8,        // 1 bytes
    status: u8,            // 1 bytes
    _scan_number: [u8; 2], // 1 byte (HALO and newer), 2 bytes (4G and older)
    _mark: [u8; 2],        // 2 bytes
    large_range: [u8; 2],  // 2 bytes, on 4G and up
    angle: [u8; 2],        // 2 bytes
    heading: [u8; 2],      // 2 bytes heading with RI-10/11. See bitmask explanation above.
    small_range: [u8; 2],  // 2 bytes or -1
    _rotation: [u8; 2],    // 2 bytes or -1
    _u01: [u8; 4],         // 4 bytes signed integer, always -1
    _u02: [u8; 4], // 4 bytes signed integer, mostly -1 (0x80 in last byte) or 0xa0 in last byte
} /* total size = 24 */

#[derive(Debug, Clone, Copy)]
#[repr(packed)]
struct RadarLine {
    _header: Br4gHeader, // or Br24Header
    _data: [u8; RADAR_LINE_DATA_LENGTH],
}

#[repr(packed)]
struct FrameHeader {
    _frame_hdr: [u8; 8],
}

#[repr(packed)]
struct RadarFramePkt {
    _header: FrameHeader,
    _line: [RadarLine; SPOKES_PER_FRAME], //  scan lines, or spokes
}

const FRAME_HEADER_LENGTH: usize = size_of::<FrameHeader>();
const RADAR_LINE_HEADER_LENGTH: usize = size_of::<Br4gHeader>();
const RADAR_LINE_LENGTH: usize = size_of::<RadarLine>();

// The LookupSpokeEnum is an index into an array, really
enum LookupDoppler {
    LowNormal = 0,
    LowBoth = 1,
    LowApproaching = 2,
    HighNormal = 3,
    HighBoth = 4,
    HighApproaching = 5,
}
const LOOKUP_DOPPLER_LENGTH: usize = (LookupDoppler::HighApproaching as usize) + 1;

type PixelToBlobType = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

fn pixel_to_blob(legend: &Legend) -> [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] {
    let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] =
        [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];
    // Cannot use for() in const expr, so use while instead
    let mut j: usize = 0;
    while j < BYTE_LOOKUP_LENGTH {
        let low: u8 = (j as u8) & 0x0f;
        let high: u8 = ((j as u8) >> 4) & 0x0f;

        lookup[LookupDoppler::LowNormal as usize][j] = low;
        lookup[LookupDoppler::LowBoth as usize][j] = match low {
            0x0f => legend.doppler_approaching,
            0x0e => legend.doppler_receding,
            _ => low,
        };
        lookup[LookupDoppler::LowApproaching as usize][j] = match low {
            0x0f => legend.doppler_approaching,
            _ => low,
        };
        lookup[LookupDoppler::HighNormal as usize][j] = high;
        lookup[LookupDoppler::HighBoth as usize][j] = match high {
            0x0f => legend.doppler_approaching,
            0x0e => legend.doppler_receding,
            _ => high,
        };
        lookup[LookupDoppler::HighApproaching as usize][j] = match high {
            0x0f => legend.doppler_approaching,
            _ => high,
        };
        j += 1;
    }
    lookup
}

pub struct NavicoReportReceiver {
    common: CommonRadar,
    transmit_after_range_detection: bool,
    report_buf: Vec<u8>,
    report_socket: Option<UdpSocket>,
    info_buf: Vec<u8>,
    info_socket: Option<UdpSocket>,
    speed_buf: Vec<u8>,
    speed_socket: Option<UdpSocket>,
    model: Model,
    command_sender: Option<Command>,
    info_sender: Option<Information>,
    range_timeout: Instant,
    info_request_timeout: Instant,
    report_request_timeout: Instant,
    reported_unknown: [bool; 256],

    // For data (spokes)
    data_buf: Vec<u8>,
    data_socket: Option<UdpSocket>,
    doppler: DopplerMode,
    pixel_to_blob: PixelToBlobType,
    prev_angle: u16,
}

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

// When others send INFO reports, we do not want to send our own INFO reports
const INFO_BY_OTHERS_TIMEOUT: Duration = Duration::from_secs(15);

// When we send INFO reports, the interval is short
const INFO_BY_US_INTERVAL: Duration = Duration::from_millis(250);

// When we are detecting ranges, we wait for 2 seconds before we send the next range
const RANGE_DETECTION_INTERVAL: Duration = Duration::from_secs(2);

// Used when we don't want to wait for something, we use now plus this
const FAR_FUTURE: Duration = Duration::from_secs(86400 * 365 * 30);

#[derive(Debug)]
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
            let report: [u8; 18] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

const REPORT_01_C4_18: u8 = 0x01;

#[derive(Debug)]
#[repr(packed)]
struct RadarReport2_99 {
    _what: u8,
    _command: u8,
    range: [u8; 4],             // 2..6 = range
    _u00: [u8; 1],              // 6
    mode: u8,                   // 7 = mode
    gain_auto: u8,              // 8
    _u01: [u8; 3],              // 9..12
    gain: u8,                   // 12
    sea_auto: u8,               // 13 = sea_auto, 0 = off, 1 = harbor, 2 = offshore
    _u02: [u8; 3],              // 14..17
    sea: [u8; 4],               // 17..21
    _u03: u8,                   // 21
    rain: u8,                   // 22
    _u04: [u8; 11],             // 23..34
    interference_rejection: u8, // 34
    _u05: [u8; 3],              // 35..38
    target_expansion: u8,       // 38
    _u06: [u8; 3],              // 39..42
    target_boost: u8,           // 42
    _u07: [u8; 56],             // 43..99
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

#[derive(Debug)]
#[repr(packed)]
struct RadarReport3_129 {
    _what: u8,
    _command: u8,
    model: u8,      // So far: 01 = 4G and new 3G, 08 = 3G, 0E and 0F = BR24, 00 = HALO
    _u00: [u8; 31], // Lots of unknown
    hours: [u8; 4], // Hours of operation
    _u01: [u8; 20], // Lots of unknown
    firmware_date: [u8; 32], // Wide chars, assumed UTF16
    firmware_time: [u8; 32], // Wide chars, assumed UTF16
    _u02: [u8; 7],
}

impl RadarReport3_129 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 129] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

const REPORT_03_C4_129: u8 = 0x03;

#[derive(Debug)]
#[repr(packed)]
struct RadarReport4_66 {
    _what: u8,
    _command: u8,
    _u00: [u8; 4],              // 2..6
    bearing_alignment: [u8; 2], // 6..8
    _u01: [u8; 2],              // 8..10
    antenna_height: [u8; 2],    // 10..12 = Antenna height
    _u02: [u8; 7],              // 12..19
    accent_light: u8,           // 19 = Accent light
    _u03: [u8; 46],             // 20..66
}

impl RadarReport4_66 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 66] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_04_C4_66: u8 = 0x04;

#[derive(Debug, Copy, Clone)]
#[repr(packed)]
struct SectorBlankingReport {
    enabled: u8,
    start_angle: [u8; 2],
    end_angle: [u8; 2],
}

#[derive(Debug)]
#[repr(packed)]
struct RadarReport6_68 {
    _what: u8,
    _command: u8,
    _u00: [u8; 4],                       // 2..6
    name: [u8; 6],                       // 6..12
    _u01: [u8; 24],                      // 12..36
    blanking: [SectorBlankingReport; 4], // 36..56
    _u02: [u8; 12],                      // 56..68
}

impl RadarReport6_68 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 68] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}
#[derive(Debug)]
#[repr(packed)]
struct RadarReport6_74 {
    _what: u8,
    _command: u8,
    _u00: [u8; 4],                       // 2..6
    name: [u8; 6],                       // 6..12
    _u01: [u8; 30],                      // 12..42
    blanking: [SectorBlankingReport; 4], // 42..52
    _u0: [u8; 12],                       // 62..74
}

impl RadarReport6_74 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 74] = bytes.try_into()?;
            transmute(report)
        })
    }
}

const REPORT_06_C4_68: u8 = 0x06;

#[derive(Debug, Copy, Clone)]
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

#[derive(Debug)]
#[repr(packed)]
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
            let report: [u8; 18] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

impl RadarReport8_21 {
    fn transmute(bytes: &[u8]) -> Result<Self, anyhow::Error> {
        // This is safe as the struct's bits are always all valid representations,
        // or we convert them using a fail safe function
        Ok(unsafe {
            let report: [u8; 21] = bytes.try_into()?; // Hardwired length on purpose to verify length
            transmute(report)
        })
    }
}

const REPORT_08_C4_18_OR_21_OR_22: u8 = 0x08;

impl NavicoReportReceiver {
    pub fn new(
        session: Session,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        model: Model,
    ) -> NavicoReportReceiver {
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
        let info_sender = if !replay {
            log::debug!("{}: Starting info sender", key);
            Some(Information::new(key.clone(), &info))
        } else {
            log::debug!("{}: No info sender, replay mode", key);
            None
        };

        let control_update_rx = info.controls.control_update_subscribe();

        let pixel_to_blob = pixel_to_blob(&info.legend);
        let trails = TrailBuffer::new(session.clone(), &info);

        let common = CommonRadar::new(key, info, radars, trails, control_update_rx, replay);

        let now = Instant::now();
        NavicoReportReceiver {
            common,
            transmit_after_range_detection: false,
            report_buf: Vec::with_capacity(1000),
            report_socket: None,
            info_buf: Vec::with_capacity(::core::mem::size_of::<HaloHeadingPacket>()),
            info_socket: None,
            speed_buf: Vec::with_capacity(::core::mem::size_of::<HaloSpeedPacket>()),
            speed_socket: None,
            model,
            command_sender,
            info_sender,
            range_timeout: now + FAR_FUTURE,
            info_request_timeout: now,
            report_request_timeout: now,
            reported_unknown: [false; 256],
            data_buf: Vec::with_capacity(size_of::<RadarFramePkt>()),
            data_socket: None,
            doppler: DopplerMode::None,
            pixel_to_blob,
            prev_angle: 0,
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_report_socket()?;
        self.socket_loop(&subsys).await
    }

    fn start_report_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(&self.common.info.report_addr, &self.common.info.nic_addr)
        {
            Ok(socket) => {
                self.report_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    fn start_info_socket(&mut self) -> io::Result<()> {
        if self.info_socket.is_some() {
            return Ok(()); // Already started
        }
        match create_udp_multicast_listen(&NAVICO_INFO_ADDRESS, &self.common.info.nic_addr) {
            Ok(socket) => {
                self.info_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for info reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    fn start_speed_socket(&mut self) -> io::Result<()> {
        if self.speed_socket.is_some() {
            return Ok(()); // Already started
        }
        match create_udp_multicast_listen(&NAVICO_SPEED_ADDRESS_A, &self.common.info.nic_addr) {
            Ok(socket) => {
                self.speed_socket = Some(socket);
                log::debug!(
                    "{}: {} via {}: listening for speed reports",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    fn start_data_socket(&mut self) -> io::Result<()> {
        if self.data_socket.is_some() {
            return Ok(()); // Already started
        }
        match create_udp_multicast_listen(
            &self.common.info.spoke_data_addr,
            &self.common.info.nic_addr,
        ) {
            Ok(sock) => {
                self.data_socket = Some(sock);
                log::debug!(
                    "{} via {}: listening for spoke data",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} via {}: create multicast failed: {}",
                    &self.common.info.spoke_data_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Err(e)
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);

        loop {
            if !self.common.replay {
                self.start_info_socket()?;
                self.start_speed_socket()?;
            }
            self.start_data_socket()?;

            let timeout = min(
                min(self.report_request_timeout, self.range_timeout),
                self.info_request_timeout,
            );

            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::debug!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },

                _ = sleep_until(timeout) => {
                    let now = Instant::now();
                    if self.range_timeout <= now {
                        self.process_range(0).await?;
                    }
                    if self.report_request_timeout <= now {
                        self.send_report_requests().await?;
                    }
                    if self.info_request_timeout <= now {
                        self.send_info_requests().await?;
                    }
                },

                r = self.report_socket.as_ref().unwrap().recv_buf_from(&mut self.report_buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if let Err(e) = self.process_report().await {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            self.report_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.info_socket.as_ref().unwrap().recv_buf_from(&mut self.info_buf),
                    if self.info_socket.is_some() => {
                    match r {
                        Ok((_len, addr)) => {
                            self.process_info(&addr);
                            self.info_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive info error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },


                r = self.speed_socket.as_ref().unwrap().recv_buf_from(&mut self.speed_buf),
                    if self.speed_socket.is_some() => {
                    match r {
                        Ok((_len, addr)) => {
                            self.process_speed(&addr);
                            self.speed_buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive speed error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.data_socket.as_ref().unwrap().recv_buf_from(&mut self.data_buf)  => {
                    match r {
                        Ok(_) => {
                            self.process_frame();
                        },
                        Err(e) => {
                            return Err(RadarError::Io(e));
                        }
                    }
                },

                r = self.common.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cu) => {let _ = self.common.process_control_update(cu, &mut self.command_sender).await;},
                    }
                }


            }
        }
    }

    fn process_frame(&mut self) {
        if self.data_buf.len() < FRAME_HEADER_LENGTH + RADAR_LINE_LENGTH {
            log::warn!(
                "UDP data frame with even less than one spoke, len {} dropped",
                self.data_buf.len()
            );
            return;
        }

        let mut spokes_in_frame = (self.data_buf.len() - FRAME_HEADER_LENGTH) / RADAR_LINE_LENGTH;
        if spokes_in_frame != 32 {
            self.common.statistics.broken_packets += 1;
            if spokes_in_frame > 32 {
                spokes_in_frame = 32;
            }
        }

        log::trace!("Received UDP frame with {} spokes", &spokes_in_frame);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok();

        let mut mark_full_rotation = false;
        let mut message = RadarMessage::new();
        message.radar = self.common.info.id as u32;

        let mut offset: usize = FRAME_HEADER_LENGTH;
        for scanline in 0..spokes_in_frame {
            let header_slice = &self.data_buf[offset..offset + RADAR_LINE_HEADER_LENGTH];
            let spoke_slice = &self.data_buf[offset + RADAR_LINE_HEADER_LENGTH
                ..offset + RADAR_LINE_HEADER_LENGTH + RADAR_LINE_DATA_LENGTH];

            if let Some((range, angle, heading)) =
                validate_header(&self.common.info, header_slice, scanline)
            {
                log::trace!("range {} angle {} heading {:?}", range, angle, heading);
                log::trace!(
                    "Received {:04} spoke {}",
                    scanline,
                    PrintableSpoke::new(spoke_slice)
                );
                let mut spoke = to_protobuf_spoke(
                    &self.common.info,
                    range,
                    angle,
                    heading,
                    now,
                    self.process_spoke(spoke_slice),
                );
                self.common
                    .trails
                    .update_trails(&mut spoke, &self.common.info.legend);
                message.spokes.push(spoke);

                if angle < self.prev_angle {
                    mark_full_rotation = true;
                }
                if ((self.prev_angle + 1) % NAVICO_SPOKES as u16) != angle {
                    self.common.statistics.missing_spokes +=
                        (angle + NAVICO_SPOKES as u16 - self.prev_angle - 1) as usize
                            % NAVICO_SPOKES as usize;
                    log::trace!("{}: Spoke angle {} is not consecutive to previous angle {}, new missing spokes {}",
                        self.common.key, angle, self.prev_angle, self.common.statistics.missing_spokes);
                }
                self.common.statistics.received_spokes += 1;
                self.prev_angle = angle;
            } else {
                log::warn!("Invalid spoke: header {:02X?}", &header_slice);
                self.common.statistics.broken_packets += 1;
            }

            offset += RADAR_LINE_LENGTH;
        }

        if mark_full_rotation {
            let ms = self.common.info.full_rotation();
            self.common.trails.set_rotation_speed(ms);
            self.common.statistics.full_rotation(&self.common.key);
        }

        self.common.info.broadcast_radar_message(message);
    }

    fn process_spoke(&self, spoke: &[u8]) -> GenericSpoke {
        let pixel_to_blob = &self.pixel_to_blob;

        // Convert the spoke data to bytes
        let mut generic_spoke: Vec<u8> = Vec::with_capacity(NAVICO_SPOKE_LEN);
        let low_nibble_index = (match self.doppler {
            DopplerMode::None => LookupDoppler::LowNormal,
            DopplerMode::Both => LookupDoppler::LowBoth,
            DopplerMode::Approaching => LookupDoppler::LowApproaching,
        }) as usize;
        let high_nibble_index = (match self.doppler {
            DopplerMode::None => LookupDoppler::HighNormal,
            DopplerMode::Both => LookupDoppler::HighBoth,
            DopplerMode::Approaching => LookupDoppler::HighApproaching,
        }) as usize;

        for pixel in spoke {
            let pixel = *pixel as usize;
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        if self.common.replay {
            // Generate circle at extreme range
            let pixel = 0xff as usize;
            generic_spoke.pop();
            generic_spoke.pop();
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        generic_spoke
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        if let Some(command_sender) = &mut self.command_sender {
            command_sender.send_report_requests().await?;
        }
        self.report_request_timeout += REPORT_REQUEST_INTERVAL;
        Ok(())
    }

    async fn send_info_requests(&mut self) -> Result<(), RadarError> {
        if let Some(info_sender) = &mut self.info_sender {
            info_sender.send_info_requests().await?;
        }
        self.info_request_timeout += INFO_BY_US_INTERVAL;
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
                        "{}: Control '{}' new value {} auto {:?} enabled {:?}",
                        self.common.key,
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

    // If range detection is in progress, go to the next range
    async fn process_range(&mut self, range: i32) -> Result<(), RadarError> {
        let range = range / 10;
        if self.common.info.ranges.len() == 0
            && self.common.info.range_detection.is_none()
            && !self.common.replay
        {
            if let Some(status) = self.common.info.controls.get_status() {
                if status == Status::Transmit {
                    log::warn!(
                        "{}: No ranges available, but radar is transmitting, standby during range detection",
                        self.common.key
                    );
                    self.send_status(Status::Standby).await?;
                    self.transmit_after_range_detection = true;
                }
            } else {
                log::warn!(
                    "{}: No ranges available and no radar status found, cannot start range detection",
                    self.common.key
                );
                return Ok(());
            }
            if let Some(control) = self.common.info.controls.get(&ControlType::Range) {
                self.common.info.range_detection = Some(RangeDetection::new(
                    self.common.key.clone(),
                    50,
                    control.item().max_value.unwrap() as i32,
                    true,
                    true,
                ));
            }
        }

        if let Some(range_detection) = &mut self.common.info.range_detection {
            match range_detection.found_range(range) {
                RangeDetectionResult::NoRange => {
                    return Ok(());
                }
                RangeDetectionResult::Complete(ranges, saved_range) => {
                    self.common.info.ranges = ranges.clone();
                    self.common
                        .info
                        .controls
                        .set_valid_ranges(&ControlType::Range, &ranges)?;
                    self.common.info.range_detection = None;
                    self.range_timeout = Instant::now() + FAR_FUTURE;

                    self.common.radars.update(&self.common.info);

                    self.send_range(saved_range).await?;
                    if self.transmit_after_range_detection {
                        self.transmit_after_range_detection = false;
                        self.send_status(Status::Transmit).await?;
                    }
                }
                RangeDetectionResult::NextRange(r) => {
                    self.range_timeout = Instant::now() + RANGE_DETECTION_INTERVAL;

                    self.send_range(r).await?;
                }
            }
        }

        Ok(())
    }

    async fn send_status(&mut self, status: Status) -> Result<(), RadarError> {
        let cv = ControlValue::new(ControlType::Status, (status as i32).to_string());
        self.command_sender
            .as_mut()
            .unwrap() // Safe, as we only create a range detection when replay is false
            .set_control(&cv, &self.common.info.controls)
            .await?;
        Ok(())
    }

    async fn send_range(&mut self, range: i32) -> Result<(), RadarError> {
        let cv: ControlValue = ControlValue::new(ControlType::Range, range.to_string());
        self.command_sender
            .as_mut()
            .unwrap() // Safe, as we only create a range detection when replay is false
            .set_control(&cv, &self.common.info.controls)
            .await?;
        Ok(())
    }

    fn process_info(&mut self, addr: &SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            if addr.ip() == &self.common.info.nic_addr {
                log::trace!(
                    "{}: Ignoring info from ourselves ({})",
                    self.common.key,
                    addr
                );
            } else {
                log::trace!(
                    "{}: {} is sending information updates",
                    self.common.key,
                    addr
                );
                self.info_request_timeout = Instant::now() + INFO_BY_OTHERS_TIMEOUT;

                if self.info_buf.len() >= ::core::mem::size_of::<HaloNavigationPacket>() {
                    if self.info_buf[36] == 0x02 {
                        if let Ok(report) = HaloNavigationPacket::transmute(&self.info_buf) {
                            let sog = u16::from_le_bytes(report.sog) as f64 * 0.01 * MS_TO_KN;
                            let cog = u16::from_le_bytes(report.cog) as f64 * 360.0 / 63488.0;
                            log::debug!(
                                "{}: Halo sog={sog} cog={cog} from navigation report {:?}",
                                self.common.key,
                                report
                            );
                        }
                    } else {
                        if let Ok(report) = HaloHeadingPacket::transmute(&self.info_buf) {
                            log::debug!("{}: Halo heading report {:?}", self.common.key, report);
                        }
                    }
                }
            }
        }
    }

    fn process_speed(&mut self, addr: &SocketAddr) {
        if let SocketAddr::V4(addr) = addr {
            if addr.ip() != &self.common.info.nic_addr {
                if let Ok(report) = HaloSpeedPacket::transmute(&self.speed_buf) {
                    log::debug!("{}: Halo speed report {:?}", self.common.key, report);
                }
            }
        }
    }

    async fn process_report(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

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
                        log::trace!("Unknown report 0x{:02x} 0xc6: {:02X?}", data[0], data);
                    }
                }
            } else {
                log::trace!("Unknown report {:02X?} dropped", data)
            }
            return Ok(());
        }
        let report_identification = data[0];
        match report_identification {
            REPORT_01_C4_18 => {
                return self.process_report_01().await;
            }
            REPORT_02_C4_99 => {
                if self.model != Model::Unknown {
                    return self.process_report_02().await;
                }
            }
            REPORT_03_C4_129 => {
                return self.process_report_03().await;
            }
            REPORT_04_C4_66 => {
                return self.process_report_04().await;
            }
            REPORT_06_C4_68 => {
                if self.model != Model::Unknown {
                    if data.len() == 68 {
                        return self.process_report_06_68().await;
                    }
                    return self.process_report_06_74().await;
                }
            }
            REPORT_08_C4_18_OR_21_OR_22 => {
                if self.model != Model::Unknown {
                    return self.process_report_08().await;
                }
            }
            _ => {
                if !self.reported_unknown[report_identification as usize] {
                    self.reported_unknown[report_identification as usize] = true;
                    log::trace!(
                        "Unknown report identification {} len {} data {:02X?} dropped",
                        report_identification,
                        data.len(),
                        data
                    );
                }
            }
        }
        Ok(())
    }

    async fn process_report_01(&mut self) -> Result<(), Error> {
        let report = RadarReport1_18::transmute(&self.report_buf)?;

        log::debug!("{}: report {:?}", self.common.key, report);

        self.set_status(report.status)
    }

    fn set_status(&mut self, status: u8) -> Result<(), Error> {
        let status = match status {
            0 => Status::Off,
            1 => Status::Standby,
            2 => Status::Transmit,
            5 => Status::Preparing,
            _ => {
                bail!("{}: Unknown radar status {}", self.common.key, status);
            }
        };
        self.set_value(&ControlType::Status, status as i32 as f32);
        Ok(())
    }

    async fn process_report_02(&mut self) -> Result<(), Error> {
        let report = RadarReport2_99::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let mode = report.mode as i32;
        let range = i32::from_le_bytes(report.range);
        let gain_auto: u8 = report.gain_auto;
        let gain = report.gain as i32;
        let sea_auto = report.sea_auto;
        let sea = i32::from_le_bytes(report.sea);
        let rain = report.rain as i32;
        let interference_rejection = report.interference_rejection as i32;
        let target_expansion = report.target_expansion as i32;
        let target_boost = report.target_boost as i32;

        self.set_value(&ControlType::Range, range as f32);
        if self.model == Model::HALO {
            self.set_value(&ControlType::Mode, mode as f32);
        }
        self.set_value_auto(&ControlType::Gain, gain as f32, gain_auto);
        if self.model != Model::HALO {
            self.set_value_auto(&ControlType::Sea, sea as f32, sea_auto);
        } else {
            self.common
                .info
                .controls
                .set_auto_state(&ControlType::Sea, sea_auto > 0)
                .unwrap(); // Only crashes if control not supported which would be an internal bug
        }
        self.set_value(&ControlType::Rain, rain as f32);
        self.set_value(
            &ControlType::InterferenceRejection,
            interference_rejection as f32,
        );
        self.set_value(&ControlType::TargetExpansion, target_expansion as f32);
        self.set_value(&ControlType::TargetBoost, target_boost as f32);

        self.process_range(range).await?;

        Ok(())
    }

    async fn process_report_03(&mut self) -> Result<(), Error> {
        let report = RadarReport3_129::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let model_raw = report.model;
        let hours = i32::from_le_bytes(report.hours);
        let firmware_date = c_wide_string(&report.firmware_date);
        let firmware_time = c_wide_string(&report.firmware_time);
        let model = Model::from(model_raw);
        match model {
            Model::Unknown => {
                if !self.reported_unknown[model_raw as usize] {
                    self.reported_unknown[model_raw as usize] = true;
                    log::error!(
                        "{}: Unknown radar model 0x{:02x}",
                        self.common.key,
                        model_raw
                    );
                }
            }
            _ => {
                if self.model != model {
                    log::info!("{}: Radar is model {}", self.common.key, model);
                    let info2 = self.common.info.clone();
                    self.model = model;
                    super::settings::update_when_model_known(
                        &mut self.common.info.controls,
                        model,
                        &info2,
                    );
                    self.common.info.set_doppler(model == Model::HALO);

                    self.common.radars.update(&self.common.info);
                }
            }
        }

        let firmware = format!("{} {}", firmware_date, firmware_time);
        self.set_value(&ControlType::OperatingHours, hours as f32);
        self.set_string(&ControlType::FirmwareVersion, firmware);

        Ok(())
    }

    async fn process_report_04(&mut self) -> Result<(), Error> {
        let report = RadarReport4_66::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        self.set_value(
            &ControlType::BearingAlignment,
            i16::from_le_bytes(report.bearing_alignment) as f32,
        );
        self.set_value(
            &ControlType::AntennaHeight,
            u16::from_le_bytes(report.antenna_height) as f32,
        );
        if self.model == Model::HALO {
            self.set_value(&ControlType::AccentLight, report.accent_light as f32);
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 2006
    ///
    async fn process_report_06_68(&mut self) -> Result<(), Error> {
        let report = RadarReport6_68::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let name = c_string(&report.name);
        self.set_string(&ControlType::ModelName, name.unwrap_or("").to_string());

        for (i, start, end) in super::BLANKING_SETS {
            let blanking = &report.blanking[i];
            let start_angle = i16::from_le_bytes(blanking.start_angle);
            let end_angle = i16::from_le_bytes(blanking.end_angle);
            let enabled = Some(blanking.enabled > 0);
            self.common.info.controls.set_value_auto_enabled(
                &start,
                start_angle as f32,
                None,
                enabled,
            )?;
            self.common.info.controls.set_value_auto_enabled(
                &end,
                end_angle as f32,
                None,
                enabled,
            )?;
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 24 (Firmware 2023)
    ///
    async fn process_report_06_74(&mut self) -> Result<(), Error> {
        let report = RadarReport6_74::transmute(&self.report_buf)?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let name = c_string(&report.name);
        // self.set_string(&ControlType::ModelName, name.unwrap_or("").to_string());
        log::debug!(
            "Radar name '{}' model '{}'",
            name.unwrap_or("null"),
            self.model
        );

        for (i, start, end) in super::BLANKING_SETS {
            let blanking = &report.blanking[i];
            let start_angle = i16::from_le_bytes(blanking.start_angle);
            let end_angle = i16::from_le_bytes(blanking.end_angle);
            let enabled = Some(blanking.enabled > 0);
            self.common.info.controls.set_value_auto_enabled(
                &start,
                start_angle as f32,
                None,
                enabled,
            )?;
            self.common.info.controls.set_value_auto_enabled(
                &end,
                end_angle as f32,
                None,
                enabled,
            )?;
        }

        Ok(())
    }

    async fn process_report_08(&mut self) -> Result<(), Error> {
        let data = &self.report_buf;

        if data.len() != size_of::<RadarReport8_18>()
            && data.len() != size_of::<RadarReport8_21>()
            && data.len() != size_of::<RadarReport8_21>() + 1
        {
            bail!(
                "{}: Report 0x08C4 invalid length {}",
                self.common.key,
                data.len()
            );
        }

        let report = RadarReport8_18::transmute(&data[0..size_of::<RadarReport8_18>()])?;

        log::trace!("{}: report {:?}", self.common.key, report);

        let sea_state = report.sea_state as i32;
        let local_interference_rejection = report.interference_rejection as i32;
        let scan_speed = report.scan_speed as i32;
        let sidelobe_suppression_auto = report.sls_auto;
        let sidelobe_suppression = report.side_lobe_suppression as i32;
        let noise_reduction = report.noise_rejection as i32;
        let target_sep = report.target_sep as i32;
        let sea_clutter = report.sea_clutter as i32;
        let auto_sea_clutter = report.auto_sea_clutter;

        // There are reports of size 21, but also 22. HALO new firmware sends 22. The last byte content is unknown.
        if data.len() >= size_of::<RadarReport8_21>() {
            let report = RadarReport8_21::transmute(&data[0..size_of::<RadarReport8_21>()])?;

            log::trace!("{}: report {:?}", self.common.key, report);

            let doppler_speed = u16::from_le_bytes(report.doppler_speed);
            let doppler_state = report.doppler_state;

            let doppler_mode: Result<DopplerMode, _> = doppler_state.try_into();
            match doppler_mode {
                Err(_) => {
                    bail!(
                        "{}: Unknown doppler state {}",
                        self.common.key,
                        report.doppler_state
                    );
                }
                Ok(doppler_mode) => {
                    log::debug!(
                        "{}: doppler mode={} speed={}",
                        self.common.key,
                        doppler_mode,
                        doppler_speed
                    );
                    self.doppler = doppler_mode;
                }
            }
            self.set_value(&ControlType::Doppler, doppler_state as f32);
            self.set_value(&ControlType::DopplerSpeedThreshold, doppler_speed as f32);
        }

        if self.model == Model::HALO {
            self.set_value(&ControlType::SeaState, sea_state as f32);
            self.set_value_with_many_auto(
                &ControlType::Sea,
                sea_clutter as f32,
                auto_sea_clutter as f32,
            );
        }
        self.set_value(
            &ControlType::LocalInterferenceRejection,
            local_interference_rejection as f32,
        );
        self.set_value(&ControlType::ScanSpeed, scan_speed as f32);
        self.set_value_auto(
            &ControlType::SideLobeSuppression,
            sidelobe_suppression as f32,
            sidelobe_suppression_auto,
        );
        self.set_value(&ControlType::NoiseRejection, noise_reduction as f32);
        if self.model == Model::HALO || self.model == Model::Gen4 {
            self.set_value(&ControlType::TargetSeparation, target_sep as f32);
        } else if target_sep > 0 {
            log::trace!(
                "{}: Target separation value {} not supported on model {}",
                self.common.key,
                target_sep,
                self.model
            );
        }

        Ok(())
    }
}

fn validate_header(
    radar_info: &RadarInfo,
    header_slice: &[u8],
    scanline: usize,
) -> Option<(u32, SpokeBearing, Option<u16>)> {
    match radar_info.locator_id {
        LocatorId::Gen3Plus => match deserialize::<Br4gHeader>(&header_slice) {
            Ok(header) => {
                log::trace!("Received {:04} header {:?}", scanline, header);

                validate_4g_header(&header)
            }
            Err(e) => {
                log::warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                return None;
            }
        },
        LocatorId::GenBR24 => match deserialize::<Br24Header>(&header_slice) {
            Ok(header) => {
                log::trace!("Received {:04} header {:?}", scanline, header);

                validate_br24_header(&header)
            }
            Err(e) => {
                log::warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                return None;
            }
        },
        _ => {
            panic!("Incorrect Navico type");
        }
    }
}

fn validate_4g_header(header: &Br4gHeader) -> Option<(u32, SpokeBearing, Option<u16>)> {
    if header.header_len != (RADAR_LINE_HEADER_LENGTH as u8) {
        log::warn!(
            "Spoke with illegal header length ({}) ignored",
            header.header_len
        );
        return None;
    }
    if header.status != 0x02 && header.status != 0x12 {
        log::warn!("Spoke with illegal status (0x{:x}) ignored", header.status);
        return None;
    }

    let heading = u16::from_le_bytes(header.heading);
    let angle = u16::from_le_bytes(header.angle) / 2;
    let large_range = u16::from_le_bytes(header.large_range);
    let small_range = u16::from_le_bytes(header.small_range);

    let range = if large_range == 0x80 {
        if small_range == 0xffff {
            0
        } else {
            (small_range as u32) / 4
        }
    } else {
        ((large_range as u32) * (small_range as u32)) / 512
    };

    let heading = extract_heading_value(heading);
    Some((range, angle, heading))
}

fn validate_br24_header(header: &Br24Header) -> Option<(u32, SpokeBearing, Option<u16>)> {
    if header.header_len != (RADAR_LINE_HEADER_LENGTH as u8) {
        log::warn!(
            "Spoke with illegal header length ({}) ignored",
            header.header_len
        );
        return None;
    }
    if header.status != 0x02 && header.status != 0x12 {
        log::warn!("Spoke with illegal status (0x{:x}) ignored", header.status);
        return None;
    }

    let heading = u16::from_le_bytes(header.heading);
    let angle = u16::from_le_bytes(header.angle) / 2;
    const BR24_RANGE_FACTOR: f64 = 10.0 / 1.414; // 10 m / sqrt(2)
    let range = ((u32::from_le_bytes(header.range) & 0xffffff) as f64 * BR24_RANGE_FACTOR) as u32;

    let heading = extract_heading_value(heading);

    Some((range, angle, heading))
}
