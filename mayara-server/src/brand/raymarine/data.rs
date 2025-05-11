use bincode::deserialize;
use log::{debug, trace, warn};
use protobuf::Message;
use serde::Deserialize;
use std::f64::consts::PI;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, time::Duration};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::Receiver;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;
use trail::TrailBuffer;

use crate::brand::raymarine::RAYMARINE_SPOKE_LEN;
use crate::locator::LocatorId;
use crate::network::create_udp_multicast_listen;
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::*;
use crate::settings::ControlType;
use crate::util::PrintableSpoke;

use super::{
    DataUpdate, RADAR_LINE_DATA_LENGTH, RAYMARINE_SPOKES, RAYMARINE_SPOKES_RAW, SPOKES_PER_FRAME,
};

const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

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
const HEADING_MASK: u16 = RAYMARINE_SPOKES_RAW - 1;
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

// The LookupSpokEnum is an index into an array, really
enum LookupSpokeEnum {
    LowNormal = 0,
    LowBoth = 1,
    LowApproaching = 2,
    HighNormal = 3,
    HighBoth = 4,
    HighApproaching = 5,
}
const LOOKUP_SPOKE_LENGTH: usize = (LookupSpokeEnum::HighApproaching as usize) + 1;

pub struct RaymarineDataReceiver {
    key: String,
    statistics: Statistics,
    info: RadarInfo,
    sock: Option<UdpSocket>,
    rx: tokio::sync::mpsc::Receiver<DataUpdate>,
    doppler: DopplerMode,
    pixel_to_blob: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH],
    replay: bool,
    trails: TrailBuffer,
}

impl RaymarineDataReceiver {
    pub fn new(info: RadarInfo, rx: Receiver<DataUpdate>, replay: bool) -> RaymarineDataReceiver {
        let key = info.key();

        let pixel_to_blob = Self::pixel_to_blob(&info.legend);
        let mut trails =
            TrailBuffer::new(info.legend.clone(), RAYMARINE_SPOKES, RAYMARINE_SPOKE_LEN);

        if let Some(control) = info.controls.get(&ControlType::DopplerTrailsOnly) {
            if let Some(value) = control.value {
                let value = value > 0.;
                trails.set_doppler_trail_only(value);
            }
        }

        RaymarineDataReceiver {
            key,
            statistics: Statistics { broken_packets: 0 },
            info: info,
            sock: None,
            rx,
            doppler: DopplerMode::None,
            pixel_to_blob,
            replay,
            trails,
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_udp_multicast_listen(&self.info.spoke_data_addr, &self.info.nic_addr) {
            Ok(sock) => {
                self.sock = Some(sock);
                debug!(
                    "{} via {}: listening for spoke data",
                    &self.info.spoke_data_addr, &self.info.nic_addr
                );
                Ok(())
            }
            Err(e) => {
                sleep(Duration::from_millis(1000)).await;
                debug!(
                    "{} via {}: create multicast failed: {}",
                    &self.info.spoke_data_addr, &self.info.nic_addr, e
                );
                Ok(())
            }
        }
    }

    fn pixel_to_blob(legend: &Legend) -> [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH] {
        let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH] =
            [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH];
        // Cannot use for() in const expr, so use while instead
        let mut j: usize = 0;
        while j < BYTE_LOOKUP_LENGTH {
            let low: u8 = (j as u8) & 0x0f;
            let high: u8 = ((j as u8) >> 4) & 0x0f;

            lookup[LookupSpokeEnum::LowNormal as usize][j] = low;
            lookup[LookupSpokeEnum::LowBoth as usize][j] = match low {
                0x0f => legend.doppler_approaching,
                0x0e => legend.doppler_receding,
                _ => low,
            };
            lookup[LookupSpokeEnum::LowApproaching as usize][j] = match low {
                0x0f => legend.doppler_approaching,
                _ => low,
            };
            lookup[LookupSpokeEnum::HighNormal as usize][j] = high;
            lookup[LookupSpokeEnum::HighBoth as usize][j] = match high {
                0x0f => legend.doppler_approaching,
                0x0e => legend.doppler_receding,
                _ => high,
            };
            lookup[LookupSpokeEnum::HighApproaching as usize][j] = match high {
                0x0f => legend.doppler_approaching,
                _ => high,
            };
            j += 1;
        }
        lookup
    }

    async fn handle_data_update(&mut self, r: Option<DataUpdate>) -> Result<(), RadarError> {
        log::info!("Received data update: {:?}", r);
        match r {
            Some(DataUpdate::Doppler(doppler)) => {
                self.doppler = doppler;
            }
            Some(DataUpdate::Legend(legend)) => {
                self.pixel_to_blob = Self::pixel_to_blob(&legend);
                self.info.legend = legend;
            }
            Some(DataUpdate::ControlValue(reply_tx, cv)) => {
                match cv.id {
                    ControlType::ClearTrails => {
                        self.trails.clear();
                    }
                    ControlType::DopplerTrailsOnly => {
                        let value = cv.value.parse::<u16>().unwrap_or(0) > 0;
                        self.trails.set_doppler_trail_only(value);
                    }
                    ControlType::TargetTrails => {
                        let value = cv.value.parse::<u16>().unwrap_or(0);
                        self.trails.set_relative_trails_length(value);
                    }
                    ControlType::TrailsMotion => {
                        let true_motion = match cv.value.as_str() {
                            "0" => false,
                            "1" => true,
                            _ => return Err(RadarError::CannotSetControlType(cv.id)),
                        };
                        if let Err(e) = self.trails.set_trails_mode(true_motion) {
                            return self
                                .info
                                .send_error_to_controller(
                                    &reply_tx,
                                    &cv,
                                    RadarError::ControlError(e),
                                )
                                .await;
                        }
                    }
                    _ => return Err(RadarError::CannotSetControlType(cv.id)),
                };
            }
            None => {
                return Err(RadarError::Shutdown);
            }
        }
        Ok(())
    }

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        let mut buf = Vec::with_capacity(size_of::<RadarFramePkt>());

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                r = self.rx.recv() => {
                  self.handle_data_update(r).await?;
                },
                r = self.sock.as_ref().unwrap().recv_buf_from(&mut buf)  => {
                    match r {
                        Ok(_) => {
                            self.process_frame(&mut buf);
                        },
                        Err(e) => {
                            return Err(RadarError::Io(e));
                        }
                    }
                },
            }
            buf.clear();
        }
    }

    pub async fn run(mut self, subsys: SubsystemHandle) -> Result<(), RadarError> {
        self.start_socket().await.unwrap();
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
                self.start_socket().await.unwrap();
            }
        }
    }

    fn process_frame(&mut self, data: &mut Vec<u8>) {
        let mut prev_angle = 0;

        if data.len() < FRAME_HEADER_LENGTH + RADAR_LINE_LENGTH {
            warn!(
                "UDP data frame with even less than one spoke, len {} dropped",
                data.len()
            );
            return;
        }

        let mut scanlines_in_packet = (data.len() - FRAME_HEADER_LENGTH) / RADAR_LINE_LENGTH;
        if scanlines_in_packet != 32 {
            self.statistics.broken_packets += 1;
            if scanlines_in_packet > 32 {
                scanlines_in_packet = 32;
            }
        }

        trace!("Received UDP frame with {} spokes", &scanlines_in_packet);

        let mut mark_full_rotation = false;
        let mut message = RadarMessage::new();
        message.radar = 1;

        let mut offset: usize = FRAME_HEADER_LENGTH;
        for scanline in 0..scanlines_in_packet {
            let header_slice = &data[offset..offset + RADAR_LINE_HEADER_LENGTH];
            let spoke_slice = &data[offset + RADAR_LINE_HEADER_LENGTH
                ..offset + RADAR_LINE_HEADER_LENGTH + RADAR_LINE_DATA_LENGTH];

            if let Some((range, angle, heading)) = self.validate_header(header_slice, scanline) {
                trace!("range {} angle {} heading {:?}", range, angle, heading);
                trace!(
                    "Received {:04} spoke {}",
                    scanline,
                    PrintableSpoke::new(spoke_slice)
                );
                message
                    .spokes
                    .push(self.process_spoke(range, angle, heading, spoke_slice));
                if angle < prev_angle {
                    mark_full_rotation = true;
                }
                prev_angle = angle;
            } else {
                warn!("Invalid spoke: header {:02X?}", &header_slice);
                self.statistics.broken_packets += 1;
            }

            offset += RADAR_LINE_LENGTH;
        }

        if mark_full_rotation {
            let ms = self.info.full_rotation();
            self.trails.set_rotation_speed(ms);
        }

        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");

        match self.info.message_tx.send(bytes) {
            Err(e) => {
                trace!("{}: Dropping received spoke: {}", self.key, e);
            }
            Ok(count) => {
                trace!("{}: sent to {} receivers", self.key, count);
            }
        }
    }

    fn validate_header(
        &self,
        header_slice: &[u8],
        scanline: usize,
    ) -> Option<(u32, SpokeBearing, Option<u16>)> {
        match self.info.locator_id {
            LocatorId::Gen3Plus => match deserialize::<Br4gHeader>(&header_slice) {
                Ok(header) => {
                    trace!("Received {:04} header {:?}", scanline, header);

                    RaymarineDataReceiver::validate_4g_header(&header)
                }
                Err(e) => {
                    warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
            LocatorId::GenBR24 => match deserialize::<Br24Header>(&header_slice) {
                Ok(header) => {
                    trace!("Received {:04} header {:?}", scanline, header);

                    RaymarineDataReceiver::validate_br24_header(&header)
                }
                Err(e) => {
                    warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
            _ => {
                panic!("Incorrect Raymarine type");
            }
        }
    }

    fn validate_4g_header(header: &Br4gHeader) -> Option<(u32, SpokeBearing, Option<u16>)> {
        if header.header_len != (RADAR_LINE_HEADER_LENGTH as u8) {
            warn!(
                "Spoke with illegal header length ({}) ignored",
                header.header_len
            );
            return None;
        }
        if header.status != 0x02 && header.status != 0x12 {
            warn!("Spoke with illegal status (0x{:x}) ignored", header.status);
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
            warn!(
                "Spoke with illegal header length ({}) ignored",
                header.header_len
            );
            return None;
        }
        if header.status != 0x02 && header.status != 0x12 {
            warn!("Spoke with illegal status (0x{:x}) ignored", header.status);
            return None;
        }

        let heading = u16::from_le_bytes(header.heading);
        let angle = u16::from_le_bytes(header.angle) / 2;
        let range = u32::from_le_bytes(header.range);

        let heading = extract_heading_value(heading);

        Some((range, angle, heading))
    }

    fn process_spoke(
        &mut self,
        range: u32,
        angle: SpokeBearing,
        heading: Option<u16>,
        spoke: &[u8],
    ) -> Spoke {
        let pixel_to_blob = &self.pixel_to_blob;

        // Convert the spoke data to bytes
        let mut generic_spoke: Vec<u8> = Vec::with_capacity(RAYMARINE_SPOKE_LEN);
        let low_nibble_index = (match self.doppler {
            DopplerMode::None => LookupSpokeEnum::LowNormal,
            DopplerMode::Both => LookupSpokeEnum::LowBoth,
            DopplerMode::Approaching => LookupSpokeEnum::LowApproaching,
        }) as usize;
        let high_nibble_index = (match self.doppler {
            DopplerMode::None => LookupSpokeEnum::HighNormal,
            DopplerMode::Both => LookupSpokeEnum::HighBoth,
            DopplerMode::Approaching => LookupSpokeEnum::HighApproaching,
        }) as usize;

        for pixel in spoke {
            let pixel = *pixel as usize;
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        if self.replay {
            // Generate circle at extreme range
            let pixel = 0xff as usize;
            generic_spoke.pop();
            generic_spoke.pop();
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        trace!(
            "Spoke {}/{:?}/{} len {}",
            range,
            heading,
            angle,
            generic_spoke.len()
        );

        let heading = if heading.is_some() {
            heading.map(|h| (((h / 2) + angle) % (RAYMARINE_SPOKES as u16)) as u32)
        } else {
            let heading = crate::signalk::get_heading_true();
            heading.map(|h| {
                (((h * RAYMARINE_SPOKES as f64 / (2. * PI)) as u16 + angle)
                    % (RAYMARINE_SPOKES as u16)) as u32
            })
        };

        self.trails
            .update_trails(range, heading.map(|x| x as u16), angle, &mut generic_spoke);

        let mut message: RadarMessage = RadarMessage::new();
        message.radar = self.info.id as u32;
        let mut spoke = Spoke::new();
        spoke.range = range;
        spoke.angle = angle as u32;
        spoke.bearing = heading;

        (spoke.lat, spoke.lon) = crate::signalk::get_position_i64();
        spoke.time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok();
        spoke.data = generic_spoke;

        self.trails.update_trails(&mut spoke, &self.info.legend);

        spoke
    }
}
