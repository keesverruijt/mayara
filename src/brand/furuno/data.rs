use crate::brand::furuno::FURUNO_RADAR_RANGES;
use crate::network::create_udp_multicast_listen;
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::protos::RadarMessage::RadarMessage;
use crate::settings::{ControlType, DataUpdate};
use crate::util::PrintableSpoke;
use crate::{radar::*, GLOBAL_ARGS};

use core::panic;
use log::{debug, trace};
use protobuf::Message;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{io, time::Duration};
use tokio::net::UdpSocket;
use tokio::time::sleep;
use tokio_graceful_shutdown::SubsystemHandle;
use trail::TrailBuffer;

use super::{FURUNO_SPOKES, FURUNO_SPOKE_LEN};

pub struct FurunoDataReceiver {
    key: String,
    info: RadarInfo,
    sock: Option<UdpSocket>,
    data_update_rx: tokio::sync::broadcast::Receiver<DataUpdate>,

    // pixel_to_blob: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH],
    prev_spoke: Vec<u8>,
    prev_angle: u16,
    sweep_count: u16,
    trails: TrailBuffer,
}

#[derive(Debug)]
struct FurunoSpokeMetadata {
    sweep_count: u32,
    sweep_len: u32,
    encoding: u8,
    have_heading: u8,
    range: u32,
}

impl FurunoDataReceiver {
    pub fn new(info: RadarInfo) -> FurunoDataReceiver {
        let key = info.key();

        let data_update_rx = info.controls.data_update_subscribe();

        // let pixel_to_blob = Self::pixel_to_blob(&info.legend);
        let mut trails = TrailBuffer::new(&info);
        if let Some(control) = info.controls.get(&ControlType::DopplerTrailsOnly) {
            if let Some(value) = control.value {
                let value = value > 0.;
                trails.set_doppler_trail_only(value);
            }
        }

        FurunoDataReceiver {
            key,
            info,
            sock: None,
            data_update_rx,
            trails,
            prev_spoke: Vec::new(),
            prev_angle: 0,
            sweep_count: 0,
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

    async fn socket_loop(&mut self, subsys: &SubsystemHandle) -> Result<(), RadarError> {
        let mut buf = Vec::with_capacity(1500);

        let sock = self.sock.take().unwrap();

        log::debug!("Starting Furuno socket loop");
        loop {
            tokio::select! {
                _ = subsys.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                r = self.data_update_rx.recv() => {
                    match r {
                        Ok(data_update) => {
                            self.handle_data_update(data_update).await?;
                        }
                        Err(_) => {
                            panic!("data_update closed");
                        }
                    }
                },
                r = sock.recv_buf_from(&mut buf)  => {
                    log::trace!("Furuno data recv {:?}", r);
                    match r {
                        Ok((len, _)) => {
                            self.process_frame(&buf[..len]);
                        },
                        Err(e) => {
                            log::error!("Furuno data socket: {}", e);
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
                    _ => {}
                }
            } else {
                sleep(Duration::from_millis(1000)).await;
                self.start_socket().await.unwrap();
            }
        }
    }

    async fn handle_data_update(&mut self, r: DataUpdate) -> Result<(), RadarError> {
        log::info!("Received data update: {:?}", r);
        match r {
            DataUpdate::Doppler(_doppler) => {
                // self.doppler = doppler;
            }
            DataUpdate::Legend(legend) => {
                // self.pixel_to_blob = Self::pixel_to_blob(&legend);
                self.info.legend = legend;
            }
            DataUpdate::ControlValue(reply_tx, cv) => {
                match self.trails.set_control_value(&self.info.controls, &cv) {
                    Ok(()) => {
                        return Ok(());
                    }
                    Err(e) => {
                        return self
                            .info
                            .controls
                            .send_error_to_client(reply_tx, &cv, &e)
                            .await;
                    }
                };
            }
        }
        Ok(())
    }

    fn process_frame(&mut self, data: &[u8]) {
        if data.len() < 16 || data[0] != 0x02 {
            log::debug!("Dropping invalid frame");
            return;
        }

        let mut message = RadarMessage::new();
        message.radar = self.info.id as u32;

        let metadata: FurunoSpokeMetadata = Self::parse_metadata_header(&data);

        let sweep_count = metadata.sweep_count;
        let sweep_len = metadata.sweep_len as usize;
        trace!("Received UDP frame with {} spokes", &sweep_count);

        let mut message = RadarMessage::new();
        message.radar = self.info.id as u32;

        let mut sweep: &[u8] = &data[16..];
        for sweep_idx in 0..sweep_count {
            if sweep.len() < 5 {
                log::error!("Unsufficient data for sweep {}", sweep_idx);
                break;
            }
            let angle = ((sweep[1] as u16) << 8) | sweep[0] as u16;
            let heading = ((sweep[3] as u16) << 8) | sweep[2] as u16;
            sweep = &sweep[4..];

            let (generic_spoke, used) = match metadata.encoding {
                0 => Self::decode_sweep_encoding_0(sweep),
                1 => Self::decode_sweep_encoding_1(sweep, sweep_len),
                2 => {
                    if sweep_idx == 0 {
                        Self::decode_sweep_encoding_1(sweep, sweep_len)
                    } else {
                        Self::decode_sweep_encoding_2(sweep, self.prev_spoke.as_slice(), sweep_len)
                    }
                }
                3 => Self::decode_sweep_encoding_3(sweep, self.prev_spoke.as_slice(), sweep_len),
                _ => {
                    panic!("Impossible encoding value")
                }
            };
            sweep = &sweep[used..];

            message
                .spokes
                .push(self.create_spoke(&metadata, angle, heading, &generic_spoke));

            self.sweep_count += 1;
            if angle < self.prev_angle {
                let ms = self.info.full_rotation();
                self.trails.set_rotation_speed(ms);
                log::trace!("sweep_count = {}", self.sweep_count);
                self.sweep_count = 0;
            }
            self.prev_angle = angle;
            self.prev_spoke = generic_spoke;
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

    fn decode_sweep_encoding_0(sweep: &[u8]) -> (Vec<u8>, usize) {
        let spoke = sweep.to_vec();

        let used = sweep.len();
        (spoke, used)
    }

    fn decode_sweep_encoding_1(sweep: &[u8], sweep_len: usize) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 1;
                if repeat == 0 {
                    repeat = 0x80;
                }

                for _ in 0..repeat {
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn decode_sweep_encoding_2(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x01 == 0 {
                let strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 1;
                if repeat == 0 {
                    repeat = 0x80;
                }

                for _ in 0..repeat {
                    let i = spoke.len();
                    let strength = if prev_spoke.len() > i {
                        prev_spoke[i]
                    } else {
                        0
                    };
                    spoke.push(strength);
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn decode_sweep_encoding_3(
        sweep: &[u8],
        prev_spoke: &[u8],
        sweep_len: usize,
    ) -> (Vec<u8>, usize) {
        let mut spoke = Vec::with_capacity(FURUNO_SPOKE_LEN);
        let mut used = 0;
        let mut strength: u8 = 0;

        while spoke.len() < sweep_len && used < sweep.len() {
            if sweep[used] & 0x03 == 0 {
                strength = sweep[used];
                spoke.push(strength);
            } else {
                let mut repeat = sweep[used] >> 2;
                if repeat == 0 {
                    repeat = 0x40;
                }

                if sweep[used] & 0x01 == 0 {
                    for _ in 0..repeat {
                        let i = spoke.len();
                        strength = if prev_spoke.len() > i {
                            prev_spoke[i]
                        } else {
                            0
                        };
                        spoke.push(strength);
                    }
                } else {
                    for _ in 0..repeat {
                        spoke.push(strength);
                    }
                }
            }
            used += 1;
        }

        used = (used + 3) & !3; // round up to int32 size
        (spoke, used)
    }

    fn create_spoke(
        &mut self,
        metadata: &FurunoSpokeMetadata,
        angle: SpokeBearing,
        heading: SpokeBearing,
        sweep: &[u8],
    ) -> Spoke {
        // Convert the spoke data to bytes

        let heading: Option<u32> = if metadata.have_heading > 0 {
            Some(heading as u32)
        } else {
            let heading = crate::navdata::get_heading_true();
            heading.map(|h| h as u32)
        };

        let mut spoke = Spoke::new();
        spoke.range = metadata.range;
        //        spoke.angle = (angle as usize * FURUNO_SPOKES / 8192) as u32;
        spoke.angle = angle as u32;
        spoke.bearing = heading;

        (spoke.lat, spoke.lon) = crate::navdata::get_position_i64();
        spoke.time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .ok();

        spoke.data = vec![0; FURUNO_SPOKE_LEN];

        let mut i = 0;
        for b in sweep {
            spoke.data[i] = b >> 2;
            i += 1;
        }
        if GLOBAL_ARGS.replay {
            spoke.data[sweep.len() - 1] = 64;
            spoke.data[sweep.len() - 2] = 64;
            spoke.data[FURUNO_SPOKE_LEN - 1] = 64;
            spoke.data[FURUNO_SPOKE_LEN - 2] = 64;
        }

        trace!(
            "Received {:04} spoke {}",
            angle,
            PrintableSpoke::new(&spoke.data)
        );

        self.trails.update_trails(&mut spoke, &self.info.legend);

        spoke
    }

    // From RadarDLLAccess RmGetEchoData() we know that the following should be in the header:
    // status, sweep_len, scale, range, angle, heading, hdg_flag.
    //
    // derived from ghidra fec/radar.dll function 'decode_sweep_2' @ 10002740
    // called from DecodeImoEchoFormat
    // Here's a typical header:
    //  [2,    #  0: 0x02 - Always 2, checked in radar.dll
    //   149,  #  1: 0x95
    //   0,
    //   1,
    //   0, 0, 0, 0,
    //   48,   #  8: 0x30
    //   17,   #  9: 0x11
    //   116,  # 10: 0x74 - low byte of sweep_len
    //   219,  # 11: 0xDB - bits 2..0 (011) = bits 10..8 of sweep_len
    //                    - bits 4..3 (11) = encoding 3
    //                    - bits 7..5 (110) = ?
    //   6,    # 12: 0x06
    //   0,    # 13: 0x00
    //   240,  # 14: 0xF0
    //   9]    # 15: 0x09
    //
    //  multi byte data: sweep_len = 0b011 << 8 | 0x74 => 0x374 = 884

    //  -> sweep_count=8 sweep_len=884 encoding=3 have_heading=0 range=496
    fn parse_metadata_header(data: &[u8]) -> FurunoSpokeMetadata {
        let sweep_count = (data[9] >> 1) as u32;
        let sweep_len = ((data[11] & 0x07) as u32) << 8 | data[10] as u32;
        let encoding = ((data[11] & 0x18) >> 3) as u8;
        let have_heading = ((data[15] & 0x30) >> 3) as u8;
        let range = FURUNO_RADAR_RANGES.get(data[12] as usize).unwrap_or(&0);
        let range = *range as u32;
        let metadata = FurunoSpokeMetadata {
            sweep_count,
            sweep_len,
            encoding,
            have_heading,
            range,
        };
        log::trace!(
            "header {:?} -> sweep_count={} sweep_len={} encoding={} have_heading={} range={}",
            &data[0..20],
            sweep_count,
            sweep_len,
            encoding,
            have_heading,
            range
        );
        metadata
    }
}
