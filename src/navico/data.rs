use bincode::deserialize;
use log::{debug, trace, warn};
use protobuf::Message;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, time::Duration};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::Receiver;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;

use crate::locator::LocatorId;
use crate::navico::NAVICO_SPOKE_LEN;
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::protos::RadarMessage::RadarMessage;
use crate::util::{create_multicast, PrintableSpoke};
use crate::{radar::*, Cli};

use super::{
    DataUpdate, NAVICO_SPOKES, NAVICO_SPOKES_RAW, RADAR_LINE_DATA_LENGTH, SPOKES_PER_FRAME,
};

const BYTE_LOOKUP_LENGTH: usize = u8::MAX as usize + 1;

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
    x & HEADING_TRUE_FLAG != 0
}
fn is_valid_heading_value(x: u16) -> bool {
    x & !(HEADING_TRUE_FLAG | HEADING_MASK) == 0
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
const LOOKUP_SPOKE_LENGTH: usize = LookupSpokeEnum::HighApproaching as usize + 1;

pub struct NavicoDataReceiver {
    key: String,
    statistics: Statistics,
    info: RadarInfo,
    buf: Vec<u8>,
    sock: Option<UdpSocket>,
    rx: tokio::sync::mpsc::Receiver<DataUpdate>,
    doppler: DopplerMode,
    legend: Option<Legend>,
    pixel_to_blob: Option<[[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH]>,
    args: Cli,
}

impl NavicoDataReceiver {
    pub fn new(info: RadarInfo, rx: Receiver<DataUpdate>, args: Cli) -> NavicoDataReceiver {
        let key = info.key();

        NavicoDataReceiver {
            key,
            statistics: Statistics { broken_packets: 0 },
            info: info,
            buf: Vec::with_capacity(size_of::<RadarFramePkt>()),
            sock: None,
            rx,
            doppler: DopplerMode::None,
            legend: None,
            pixel_to_blob: None,
            args,
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_multicast(&self.info.spoke_data_addr, &self.info.nic_addr) {
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

    fn fill_pixel_to_blob(&mut self, legend: &Legend) {
        let mut lookup: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH] =
            [[0; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH];
        // Cannot use for() in const expr, so use while instead
        let mut j: usize = 0;
        while j < BYTE_LOOKUP_LENGTH {
            let low: u8 = j as u8 & 0x0f;
            let high: u8 = (j as u8 >> 4) & 0x0f;

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
        self.pixel_to_blob = Some(lookup);
    }

    fn handle_data_update(&mut self, r: Option<DataUpdate>) {
        match r {
            Some(DataUpdate::Doppler(doppler)) => {
                self.doppler = doppler;
            }
            Some(DataUpdate::Legend(legend)) => {
                self.fill_pixel_to_blob(&legend);
                self.legend = Some(legend);
            }
            None => {}
        }
    }

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                r = self.rx.recv() => {
                  self.handle_data_update(r);
                },
                r = self.sock.as_ref().unwrap().recv_buf_from(&mut self.buf)  => {
                    match r {
                        Ok(_) => {
                            self.process_frame();
                        },
                        Err(e) => {
                            return Err(RadarError::Io(e));
                        }
                    }
                },
            };
            self.buf.clear();
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

    fn process_frame(&mut self) {
        let data = &self.buf;

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

        let mut message = RadarMessage::new();
        message.radar = 1;
        if let Some(pixel_to_blob) = self.pixel_to_blob {
            let mut offset: usize = FRAME_HEADER_LENGTH;
            for scanline in 0..scanlines_in_packet {
                let header_slice = &data[offset..offset + RADAR_LINE_HEADER_LENGTH];
                let spoke_slice = &data[offset + RADAR_LINE_HEADER_LENGTH
                    ..offset + RADAR_LINE_HEADER_LENGTH + RADAR_LINE_DATA_LENGTH];

                if let Some((range, angle, heading)) = self.validate_header(header_slice, scanline)
                {
                    trace!("range {} angle {} heading {:?}", range, angle, heading);
                    trace!(
                        "Received {:04} spoke {}",
                        scanline,
                        PrintableSpoke::new(spoke_slice),
                    );
                    message.spokes.push(self.process_spoke(
                        &pixel_to_blob,
                        range,
                        angle,
                        heading,
                        spoke_slice,
                    ));
                } else {
                    warn!("Invalid spoke: header {:02X?}", &header_slice);
                    self.statistics.broken_packets += 1;
                }
                offset += RADAR_LINE_LENGTH;
            }
        }

        let mut bytes = Vec::new();
        message
            .write_to_vec(&mut bytes)
            .expect("Cannot write RadarMessage to vec");

        match self.info.radar_message_tx.send(bytes) {
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
    ) -> Option<(u32, u16, Option<u16>)> {
        match self.info.locator_id {
            LocatorId::Gen3Plus => match deserialize::<Br4gHeader>(&header_slice) {
                Ok(header) => {
                    trace!("Received {:04} header {:?}", scanline, header);

                    NavicoDataReceiver::validate_4g_header(&header)
                }
                Err(e) => {
                    warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
            LocatorId::GenBR24 => match deserialize::<Br24Header>(&header_slice) {
                Ok(header) => {
                    trace!("Received {:04} header {:?}", scanline, header);

                    NavicoDataReceiver::validate_br24_header(&header)
                }
                Err(e) => {
                    warn!("Illegible spoke: {} header {:02X?}", e, &header_slice);
                    return None;
                }
            },
        }
    }

    fn validate_4g_header(header: &Br4gHeader) -> Option<(u32, u16, Option<u16>)> {
        if header.header_len != RADAR_LINE_HEADER_LENGTH as u8 {
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
        let angle = u16::from_le_bytes(header.angle);
        let large_range = u16::from_le_bytes(header.large_range);
        let small_range = u16::from_le_bytes(header.small_range);

        let range = if large_range == 0x80 {
            if small_range == 0xffff {
                0
            } else {
                small_range as u32 / 4
            }
        } else {
            large_range as u32 * small_range as u32 / 512
        };

        let heading = extract_heading_value(heading);
        Some((range, angle, heading))
    }

    fn validate_br24_header(header: &Br24Header) -> Option<(u32, u16, Option<u16>)> {
        if header.header_len != RADAR_LINE_HEADER_LENGTH as u8 {
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
        let angle = u16::from_le_bytes(header.angle);
        let range = u32::from_le_bytes(header.range);

        let heading = extract_heading_value(heading);

        Some((range, angle, heading))
    }

    fn process_spoke(
        &self,
        pixel_to_blob: &[[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH],
        range: u32,
        angle: u16,
        heading: Option<u16>,
        spoke: &[u8],
    ) -> Spoke {
        // Convert the spoke data to bytes
        let mut generic_spoke: Vec<u8> = Vec::with_capacity(NAVICO_SPOKE_LEN);
        let low_nibble_index = match self.doppler {
            DopplerMode::None => LookupSpokeEnum::LowNormal,
            DopplerMode::Both => LookupSpokeEnum::LowBoth,
            DopplerMode::Approaching => LookupSpokeEnum::LowApproaching,
        } as usize;
        let high_nibble_index = match self.doppler {
            DopplerMode::None => LookupSpokeEnum::HighNormal,
            DopplerMode::Both => LookupSpokeEnum::HighBoth,
            DopplerMode::Approaching => LookupSpokeEnum::HighApproaching,
        } as usize;

        for pixel in spoke {
            let pixel = *pixel as usize;
            generic_spoke.push(pixel_to_blob[low_nibble_index][pixel]);
            generic_spoke.push(pixel_to_blob[high_nibble_index][pixel]);
        }

        if self.args.replay {
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

        let angle = (angle / 2) as u32;
        // For now, don't send heading in replay mode, signalk-radar-client doesn't
        // handle it well yet.
        let heading = if self.args.replay {
            None
        } else {
            heading.map(|h| ((h / 2) as u32 + angle) % NAVICO_SPOKES as u32)
        };

        let mut message = RadarMessage::new();
        message.radar = 1;
        let mut spoke = Spoke::new();
        spoke.range = range;
        spoke.angle = angle;
        spoke.bearing = heading;
        spoke.time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok();
        spoke.data = generic_spoke;

        spoke
    }
}
