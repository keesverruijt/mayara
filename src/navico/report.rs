use anyhow::{bail, Error};
use enum_primitive_derive::Primitive;
use log::{debug, error, info, trace};
use std::cmp::{max, min};
use std::mem::transmute;
use std::time::Duration;
use std::{fmt, io};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::Sender;
use tokio::time::{sleep, sleep_until, Instant};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::radar::{DopplerMode, RadarError, RadarInfo, RangeDetection, SharedRadars};
use crate::settings::{ControlMessage, ControlType, ControlValue};
use crate::util::{c_string, c_wide_string, create_udp_multicast_listen};
use crate::Cli;

use super::command::Command;
use super::{DataUpdate, Model};

pub struct NavicoReportReceiver {
    info: RadarInfo,
    key: String,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
    radars: SharedRadars,
    args: Cli,
    model: Model,
    command_sender: Command,
    data_tx: Sender<DataUpdate>,
    range_timeout: Option<Instant>,
    report_request_timeout: Instant,
    report_request_interval: Duration,
    reported_unknown: [bool; 256],
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
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
        model: Model,
        data_tx: Sender<DataUpdate>,
    ) -> NavicoReportReceiver {
        let key = info.key();

        let command_sender = Command::new(info.clone(), model.clone(), radars.clone());
        let args = radars.cli_args();

        NavicoReportReceiver {
            key: key,
            info: info,
            buf: Vec::with_capacity(1000),
            sock: None,
            radars,
            args,
            model,
            command_sender,
            range_timeout: None,
            report_request_timeout: Instant::now(),
            report_request_interval: Duration::from_millis(5000),
            data_tx,
            reported_unknown: [false; 256],
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

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        debug!("{}: listening for reports", self.key);
        let mut command_rx = self.info.command_tx.subscribe();

        loop {
            let mut is_range_timeout = false;
            let mut timeout = self.report_request_timeout;
            if let Some(t) = self.range_timeout {
                if t < timeout {
                    timeout = t;
                    is_range_timeout = true;
                }
            }
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    info!("{}: shutdown", self.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(timeout) => {
                    if is_range_timeout {
                        self.process_range(0).await?;
                    } else {
                        self.send_report_requests().await?;
                    }
                },

                r = self.sock.as_ref().unwrap().recv_buf_from(&mut self.buf)  => {
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
                    match r {
                        Ok(control_message) => {
                            match self.process_control_message(&control_message).await {
                                Ok(()) => {}
                                Err(e) => {
                                    log::error!("Cannot act on control message: {e}");
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Cannot read control message: {e}");
                            // Send a JSON reply on websocket

                        }
                    }
                }
            }
        }
    }

    async fn process_control_message(
        &mut self,
        control_message: &ControlMessage,
    ) -> Result<(), RadarError> {
        match control_message {
            ControlMessage::NewClient(reply_tx) => {
                // Send all control values
                self.info.send_all_json(reply_tx.clone()).await?;
            }
            ControlMessage::Value(reply_tx, cv) => {
                // match strings first
                match cv.id {
                    ControlType::UserName => {
                        self.info
                            .set_string(&ControlType::UserName, cv.value.clone())
                            .unwrap();
                        self.radars.update(&self.info);
                        return Ok(());
                    }
                    ControlType::TargetTrails
                    | ControlType::ClearTrails
                    | ControlType::DopplerTrailsOnly => {
                        self.pass_to_data_receiver(cv).await?;
                        return Ok(());
                    }
                    _ => {} // rest is numeric
                }

                if let Err(e) = self
                    .command_sender
                    .set_control(cv, &self.info.controls)
                    .await
                {
                    // Find our current control value for this ControlType
                    if let Some(control) = self.info.controls.get(&cv.id) {
                        self.info
                            .send_json(reply_tx.clone(), control, Some(e.to_string()))
                            .await?;
                    } else {
                        log::error!("Cannot send control error back: {e}");
                    }
                } else {
                    self.info.set_refresh(&cv.id);
                }
            }
            ControlMessage::SetValue(cv) => {
                self.info.set_string(&cv.id, cv.value.clone()).unwrap();
                self.radars.update(&self.info);
                return Ok(());
            }
        }
        Ok(())
    }

    async fn pass_to_data_receiver(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        let value = cv.value.parse::<f32>().unwrap_or(0.);
        if self.info.set(&cv.id, value, cv.auto).is_err() {
            log::warn!("Cannot set {} to {}", cv.id, value);
        }

        self.data_tx
            .send(DataUpdate::ControlValue(cv.clone()))
            .await
            .map_err(|_| RadarError::CannotSetControlType(cv.id))?;
        Ok(())
    }

    async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        self.command_sender.send_report_requests().await?;
        self.report_request_timeout += self.report_request_interval;
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
        match self.info.set(control_type, value, auto) {
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
        match self.info.set_value_auto(control_type, auto > 0, value) {
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
        match self.info.set_string(control, value) {
            Err(e) => {
                error!("{}: {}", self.key, e.to_string());
            }
            Ok(Some(v)) => {
                debug!("{}: Control '{}' new value '{}'", self.key, control, v);
            }
            Ok(None) => {}
        };
    }

    // If range detection is in progress, go to the next range
    async fn process_range(&mut self, range: i32) -> Result<(), RadarError> {
        if self.info.range_detection.is_none() && !self.args.replay {
            if let Some(control) = self.info.controls.get(&ControlType::Range) {
                self.info.range_detection = Some(RangeDetection::new(
                    control.item().min_value.unwrap() as i32,
                    control.item().max_value.unwrap() as i32,
                ));
            }
        }

        if let Some(range_detection) = &mut self.info.range_detection {
            if range_detection.complete {
                return Ok(());
            }
            let mut complete = false;

            let range = range / 10; // Raw value is in decimeters

            if !range_detection.complete {
                let mut next_range = Self::next_range(max(
                    range_detection.min_range,
                    max(range, range_detection.commanded_range),
                ));
                log::debug!(
                    "{}: Range detected range={}m commanded={}m -> next {}",
                    self.key,
                    range,
                    range_detection.commanded_range,
                    next_range
                );

                if range
                    > *range_detection
                        .ranges
                        .last()
                        .unwrap_or(&(range_detection.min_range - 1))
                {
                    range_detection.ranges.push(range);
                    log::debug!(
                        "{}: Range detection ranges: {:?}",
                        self.key,
                        range_detection.ranges
                    );
                }
                if next_range > range_detection.max_range {
                    range_detection.complete = true;
                    next_range = range_detection.saved_range;
                    complete = true;
                    log::info!(
                        "{}: Range detection complete, ranges: {:?}",
                        self.key,
                        range_detection.ranges
                    );
                } else {
                    // Set a timer to pick up the range if it doesn't do anything, so we are called again...
                    self.range_timeout = Some(Instant::now() + Duration::from_secs(2));
                }
                log::trace!(
                    "{}: Range detection ask for range {}m",
                    self.key,
                    next_range
                );
                range_detection.commanded_range = next_range;
                let cv = ControlValue::new(ControlType::Range, next_range.to_string());
                self.command_sender
                    .set_control(&cv, &self.info.controls)
                    .await?;
            }

            if complete {
                if let Some(control) = self.info.controls.get_mut(&ControlType::Range) {
                    control.set_valid_values(range_detection.ranges.clone());
                }

                self.radars.update(&self.info);
            }
        }

        Ok(())
    }

    ///
    /// Come up with a next range where the result > input and result
    /// is some nice "round" number, either in NM or meters.
    ///
    fn next_range(r: i32) -> i32 {
        let metric = match r {
            i32::MIN..75 => 75,
            75..100 => 100,
            100..150 => 150,
            150..400 => r / 100 * 100 + 100,
            400..1000 => r / 200 * 200 + 200,
            1000..1500 => 1500,
            1500..4000 => r / 1000 * 1000 + 1000,
            4000.. => {
                1000 * match r / 1000 {
                    i32::MIN..6 => 6,
                    6..8 => 8,
                    8..12 => 12,
                    12..16 => 16,
                    16..24 => 24,
                    24..36 => 36,
                    36..48 => 48,
                    48..72 => 72,
                    72..96 => 96,
                    96.. => r * 96 / 64,
                }
            }
        };
        let nautical = match r {
            i32::MIN..57 => 57, // 1/32 nm
            57..114 => 114,     // 1/16 nm
            114..231 => 231,    // 1/8 nm
            231..347 => 347,    // 3/16 nm
            347..463 => 463,    // 1/4 nm
            463..693 => 693,    // 3/4 nm
            693..926 => 926,    // 1/2 nm
            926..1157 => 1157,  // 5/8 nm
            1157..1389 => 1389, // 3/4 nm
            1389..1852 => 1852, // 1 nm
            1852..2315 => 2315, // 1,25 nm
            2315..2778 => 2778, // 1,5 nm
            2778..3704 => 3704, // 2 nm
            3704..4630 => 4630, // 2,5 nm
            4630.. => {
                1852 * match r / 1852 {
                    i32::MIN..3 => 3,
                    3 => 4,
                    4 => 5,
                    5 => 6,
                    6..8 => 8,
                    8..10 => 10,
                    10..12 => 12,
                    12..15 => 15,
                    12..16 => 16,
                    16..20 => 20,
                    20..24 => 24,
                    24..30 => 30,
                    30..36 => 36,
                    36..48 => 48,
                    48..64 => 64,
                    64..72 => 72,
                    72.. => r * 72 / 48,
                }
            }
        };
        log::debug!("compute next {}: metric {} nautic {}", r, metric, nautical);
        min(metric, nautical)
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
                        return Ok(());
                    }
                    _ => {
                        bail!("Unknown report 0x{:02x} 0xc6: {:02X?}", data[0], data);
                    }
                }
            }
            bail!("Unknown report {:02X?} dropped", data);
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
                    bail!(
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
        let report = RadarReport1_18::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

        let status: Result<Status, _> = report.status.try_into();
        if status.is_err() {
            bail!("{}: Unknown radar status {}", self.key, report.status);
        }
        self.set_value(&ControlType::Status, report.status as f32);

        Ok(())
    }

    async fn process_report_02(&mut self) -> Result<(), Error> {
        let report = RadarReport2_99::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

        let mode = report.mode as i32;
        let range = i32::from_le_bytes(report.range);
        let gain_auto = report.gain_auto;
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
            self.info
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
        let report = RadarReport3_129::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

        let model = report.model;
        let hours = i32::from_le_bytes(report.hours);
        let firmware_date = c_wide_string(&report.firmware_date);
        let firmware_time = c_wide_string(&report.firmware_time);
        let model: Result<Model, _> = model.try_into();
        match model {
            Err(_) => {
                bail!("{}: Unknown model # {}", self.key, report.model);
            }
            Ok(model) => {
                if self.model != model {
                    info!("{}: Radar is model {}", self.key, model);
                    let info2 = self.info.clone();
                    self.model = model;
                    super::settings::update_when_model_known(
                        &mut self.info.controls,
                        model,
                        &info2,
                    );
                    self.info.set_legend(model == Model::HALO);

                    self.radars.update(&self.info);

                    self.data_tx
                        .send(DataUpdate::Legend(self.info.legend.clone()))
                        .await?;
                }
            }
        }

        let firmware = format!("{} {}", firmware_date, firmware_time);
        self.set_value(&ControlType::OperatingHours, hours as f32);
        self.set_string(&ControlType::FirmwareVersion, firmware);

        Ok(())
    }

    async fn process_report_04(&mut self) -> Result<(), Error> {
        let report = RadarReport4_66::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

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
        let report = RadarReport6_68::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

        let name = c_string(&report.name);
        self.set_string(&ControlType::ModelName, name.unwrap_or("").to_string());

        for (i, start, end) in super::BLANKING_SETS {
            let blanking = &report.blanking[i];
            let start_angle = i16::from_le_bytes(blanking.start_angle);
            let end_angle = i16::from_le_bytes(blanking.end_angle);
            let enabled = Some(blanking.enabled > 0);
            self.info
                .set_value_auto_enabled(&start, start_angle as f32, None, enabled)?;
            self.info
                .set_value_auto_enabled(&end, end_angle as f32, None, enabled)?;
        }

        Ok(())
    }

    ///
    /// Blanking (No Transmit) report as seen on HALO 24 (Firmware 2023)
    ///
    async fn process_report_06_74(&mut self) -> Result<(), Error> {
        let report = RadarReport6_74::transmute(&self.buf)?;

        trace!("{}: report {:?}", self.key, report);

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
            self.info
                .set_value_auto_enabled(&start, start_angle as f32, None, enabled)?;
            self.info
                .set_value_auto_enabled(&end, end_angle as f32, None, enabled)?;
        }

        Ok(())
    }

    async fn process_report_08(&mut self) -> Result<(), Error> {
        let data = &self.buf;

        if data.len() != size_of::<RadarReport8_18>()
            && data.len() != size_of::<RadarReport8_21>()
            && data.len() != size_of::<RadarReport8_21>() + 1
        {
            bail!("{}: Report 0x08C4 invalid length {}", self.key, data.len());
        }

        let report = RadarReport8_18::transmute(&data[0..size_of::<RadarReport8_18>()])?;

        trace!("{}: report {:?}", self.key, report);

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

            trace!("{}: report {:?}", self.key, report);

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
        self.set_value(&ControlType::TargetSeparation, target_sep as f32);

        Ok(())
    }
}
