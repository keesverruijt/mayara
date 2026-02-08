use anyhow::{Error, bail};
use std::collections::HashMap;
use std::io;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_graceful_shutdown::SubsystemHandle;

use crate::Cli;
use crate::brand::raymarine::RaymarineModel;
use crate::network::create_udp_multicast_listen;
use crate::radar::range::Ranges;
use crate::radar::{BYTE_LOOKUP_LENGTH, CommonRadar, Legend, RadarError, RadarInfo, SharedRadars};
use crate::settings::ControlId;

// use super::command::Command;
use super::command::Command;

mod quantum;
mod rd;

// Every 5 seconds we ask the radar for reports, so we can update our controls
const REPORT_REQUEST_INTERVAL: Duration = Duration::from_millis(5000);

// The LookupSpokeEnum is an index into an array, really
enum LookupDoppler {
    Normal = 0,
    Doppler = 1,
}
const LOOKUP_DOPPLER_LENGTH: usize = (LookupDoppler::Doppler as usize) + 1;

type PixelToBlobType = [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

pub(super) fn pixel_to_blob(legend: &Legend) -> PixelToBlobType {
    let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH] =
        [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_DOPPLER_LENGTH];

    if legend.target_colors >= 128 {
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[LookupDoppler::Normal as usize][j] = j as u8 / 2;
            lookup[LookupDoppler::Doppler as usize][j] = match j {
                0xff => legend.doppler_approaching,
                0xfe => legend.doppler_receding,
                _ => j as u8 / 2,
            };
        }
    } else {
        for j in 0..BYTE_LOOKUP_LENGTH {
            lookup[LookupDoppler::Normal as usize][j] = j as u8;
            lookup[LookupDoppler::Doppler as usize][j] = match j {
                0xff => legend.doppler_approaching,
                0xfe => legend.doppler_receding,
                _ => j as u8,
            };
        }
    }
    log::info!("Created pixel_to_blob from legend {:?}", legend);
    lookup
}

#[derive(PartialEq, PartialOrd, Debug)]
enum ReceiverState {
    Initial,
    InfoRequestReceived,
    FixedRequestReceived,
    StatusRequestReceived,
}

pub(crate) struct RaymarineReportReceiver {
    common: CommonRadar,
    report_socket: Option<UdpSocket>,
    state: ReceiverState,
    model: Option<RaymarineModel>,
    command_sender: Option<Command>,
    report_request_timeout: Instant,
    reported_unknown: HashMap<u32, bool>,

    // For data (spokes)
    range_meters: u32,
    pixel_to_blob: PixelToBlobType,
}

impl RaymarineReportReceiver {
    pub fn new(
        args: &Cli,
        info: RadarInfo, // Quick access to our own RadarInfo
        radars: SharedRadars,
    ) -> RaymarineReportReceiver {
        let key = info.key();

        let replay = args.replay;
        log::debug!(
            "{}: Creating RaymarineReportReceiver with args {:?}",
            key,
            args
        );
        let command_sender = None; // Only known after we receive the model info

        let control_update_rx = info.controls.control_update_subscribe();

        let pixel_to_blob = pixel_to_blob(&info.get_legend());

        let common = CommonRadar::new(args, key, info, radars, control_update_rx, replay);

        let now = Instant::now();
        RaymarineReportReceiver {
            common,
            report_socket: None,
            state: ReceiverState::Initial,
            model: None, // We don't know this yet, it will be set when we receive the first info report
            command_sender,
            report_request_timeout: now,
            reported_unknown: HashMap::new(),
            range_meters: 0,
            pixel_to_blob,
        }
    }

    async fn start_report_socket(&mut self) -> io::Result<()> {
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
                sleep(Duration::from_millis(1000)).await;
                log::debug!(
                    "{}: {} via {}: create multicast failed: {}",
                    self.common.key,
                    &self.common.info.report_addr,
                    &self.common.info.nic_addr,
                    e
                );
                Ok(())
            }
        }
    }

    //
    // Process reports coming in from the radar on self.sock and commands from the
    // controller (= user) on self.common.info.command_tx.
    //
    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        log::debug!("{}: listening for reports", self.common.key);
        let mut buf = Vec::with_capacity(10000);

        loop {
            let timeout = self.report_request_timeout;
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    log::info!("{}: shutdown", self.common.key);
                    return Err(RadarError::Shutdown);
                },
                _ = sleep_until(timeout) => {
                     self.send_report_requests().await?;

                },

                r = self.report_socket.as_ref().unwrap().recv_buf_from(&mut buf)  => {
                    match r {
                        Ok((_len, _addr)) => {
                            if buf.len() == buf.capacity() {
                                let old = buf.capacity();
                                buf.reserve(1024);
                                log::warn!("{}: UDP report buffer full, increasing size {} -> {}", self.common.key, old, buf.capacity()   );
                            }
                            else if let Err(e) = self.process_report(&buf).await {
                                log::error!("{}: {}", self.common.key, e);
                            }
                            buf.clear();
                        }
                        Err(e) => {
                            log::error!("{}: receive error: {}", self.common.key, e);
                            return Err(RadarError::Io(e));
                        }
                    }
                },
                r = self.common.control_update_rx.recv() => {
                    match r {
                        Err(_) => {},
                        Ok(cv) => {let _ = self.common.process_control_update(cv, &mut self.command_sender).await;},
                    }
                }
            }
        }
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
    fn set<T>(
        &mut self,
        control_id: &ControlId,
        value: T,
        auto: Option<bool>,
        enabled: Option<bool>,
    ) where
        f32: From<T>,
    {
        match self
            .common
            .info
            .controls
            .set_value_auto_enabled(control_id, value, auto, enabled)
        {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.common.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new value {} auto {:?} enabled {:?}",
                        self.common.key,
                        control_id,
                        control.value(),
                        control.auto,
                        control.enabled
                    );
                }
            }
            Ok(None) => {}
        };
    }

    fn set_value<T>(&mut self, control_id: &ControlId, value: T)
    where
        f32: From<T>,
    {
        self.set(control_id, value.into(), None, None)
    }

    fn set_value_auto<T>(&mut self, control_id: &ControlId, value: T, auto: u8)
    where
        f32: From<T>,
    {
        self.set(control_id, value, Some(auto > 0), None)
    }

    fn set_value_enabled<T>(&mut self, control_id: &ControlId, value: T, enabled: u8)
    where
        f32: From<T>,
    {
        self.set(control_id, value, None, Some(enabled > 0))
    }

    fn set_string(&mut self, control: &ControlId, value: String) {
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

    fn set_wire_range(&mut self, control_id: &ControlId, min: u8, max: u8) {
        match self
            .common
            .info
            .controls
            .set_wire_range(control_id, min as f32, max as f32)
        {
            Err(e) => {
                log::error!("{}: {}", self.common.key, e.to_string());
            }
            Ok(Some(())) => {
                if log::log_enabled!(log::Level::Debug) {
                    let control = self.common.info.controls.get(control_id).unwrap();
                    log::trace!(
                        "{}: Control '{}' new wire min {} max {} value {} auto {:?} enabled {:?} ",
                        self.common.key,
                        control_id,
                        min,
                        max,
                        control.value(),
                        control.auto,
                        control.enabled,
                    );
                }
            }
            Ok(None) => {}
        };
    }

    async fn process_report(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() < 4 {
            bail!("UDP report len {} dropped", data.len());
        }
        log::trace!("{}: UDP report {:02X?}", self.common.key, data);

        let id = u32::from_le_bytes(data[0..4].try_into().unwrap());
        match id {
            0x010001 | 0x018801 => {
                rd::process_status_report(self, data);
            }
            0x010002 => {
                rd::process_fixed_report(self, data);
            }
            0x010003 => {
                rd::process_frame(self, data);
            }
            0x010006 => {
                rd::process_info_report(self, data);
            }
            0x280001 => {
                quantum::process_info_report(self, data);
            }
            0x280002 => {
                quantum::process_status_report(self, data);
            }
            0x280003 => {
                quantum::process_frame(self, data);
            }
            0x280030 => {
                quantum::process_doppler_report(self, data);
            }
            _ => {
                if self.reported_unknown.get(&id).is_none() {
                    log::warn!("{}: Unknown report ID {:08X?}", self.common.key, id);
                    self.reported_unknown.insert(id, true);
                }
            }
        }
        Ok(())
    }

    fn set_ranges(&mut self, ranges: Ranges) {
        if self.common.info.set_ranges(ranges).is_ok() {
            if let Some(command_sender) = &mut self.command_sender {
                command_sender.set_ranges(self.common.info.ranges.clone());
            }
            self.common.update();
        }
    }
}
