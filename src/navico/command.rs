use log::{debug, trace};
use num_traits::ToPrimitive;
use std::cmp::min;
use tokio::net::UdpSocket;

use crate::radar::{RadarError, RadarInfo, SharedRadars};
use crate::settings::{ControlType, ControlValue};
use crate::util::create_multicast_send;

use super::Model;

pub const REQUEST_03_REPORT: [u8; 2] = [0x04, 0xc2]; // This causes the radar to report Report 3
pub const REQUEST_MANY2_REPORT: [u8; 2] = [0x01, 0xc2]; // This causes the radar to report Report 02, 03, 04, 07 and 08
pub const _REQUEST_04_REPORT: [u8; 2] = [0x02, 0xc2]; // This causes the radar to report Report 4
pub const _REQUEST_02_08_REPORT: [u8; 2] = [0x03, 0xc2]; // This causes the radar to report Report 2 and Report 8
const COMMAND_STAY_ON_A: [u8; 2] = [0xa0, 0xc1];

pub struct Command {
    key: String,
    info: RadarInfo,
    model: Model,
    sock: Option<UdpSocket>,
    fake_errors: bool,
}

impl Command {
    pub fn new(info: RadarInfo, model: Model, radars: SharedRadars) -> Self {
        let args = radars.cli_args();

        Command {
            key: info.key(),
            info,
            model,
            sock: None,
            fake_errors: args.fake_errors,
        }
    }

    async fn start_socket(&mut self) -> Result<(), RadarError> {
        match create_multicast_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                debug!(
                    "{} {} via {}: sending commands",
                    self.key, &self.info.send_command_addr, &self.info.nic_addr
                );
                self.sock = Some(sock);

                Ok(())
            }
            Err(e) => {
                debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key, &self.info.send_command_addr, &self.info.nic_addr, e
                );
                Err(RadarError::Io(e))
            }
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> Result<(), RadarError> {
        if self.sock.is_none() {
            self.start_socket().await?;
        }
        if let Some(sock) = &self.sock {
            sock.send(message).await.map_err(RadarError::Io)?;
            trace!("{}: sent {:02X?}", self.key, message);
        }

        Ok(())
    }

    fn mod_degrees(a: i32) -> i32 {
        (a + 720) % 360
    }

    fn near(a: i32, b: i32) -> bool {
        return a >= b - 1 && a <= b + 1 || (b == 0 && a == 99);
    }

    fn is_metric(i: i32) -> bool {
        Self::near(i, 50) || Self::near(i, 75) || Self::near(i % 100, 0)
    }

    fn valid_range(&self, i: i32) -> i32 {
        if let Some(rd) = &self.info.range_detection {
            let mut next = false;
            let metric = Self::is_metric(i);
            let mut prev = if metric { rd.ranges[0] } else { rd.ranges[1] };
            for range in &rd.ranges {
                if next && metric == Self::is_metric(*range) {
                    return *range;
                }
                if i == *range - 1 {
                    return prev;
                }
                if i == *range {
                    return i;
                }
                if i == *range + 1 {
                    next = true;
                }
                if metric == Self::is_metric(*range) {
                    prev = *range;
                }
            }

            prev
        } else {
            i
        }
    }

    fn generate_fake_error(v: i32) -> Result<(), RadarError> {
        match v {
            11 => Err(RadarError::CannotSetControlType(ControlType::Rain)),
            12 => Err(RadarError::CannotSetControlType(ControlType::Status)),
            _ => Err(RadarError::NoSuchRadar("FakeRadarKey".to_string())),
        }
    }

    pub async fn set_control(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        let value = cv
            .value
            .parse::<i32>()
            .map_err(|_| RadarError::MissingValue(cv.id))?;
        let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };

        let mut cmd = Vec::with_capacity(6);

        match cv.id {
            ControlType::Status => {
                cmd.extend_from_slice(&[0x00, 0xc1, 0x01]);
                self.send(&cmd).await?;
                cmd.clear();
                cmd.extend_from_slice(&[0x01, 0xc1, value as u8 - 1]);
            }

            ControlType::Range => {
                let decimeters: i32 = self.valid_range(value) * 10;
                log::trace!("range {value} -> {decimeters}");

                cmd.extend_from_slice(&[0x03, 0xc1]);
                cmd.extend_from_slice(&decimeters.to_le_bytes());
            }
            ControlType::BearingAlignment => {
                let value: i16 = Self::mod_degrees(value) as i16 * 10;

                cmd.extend_from_slice(&[0x05, 0xc1]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlType::Gain => {
                let v = min((value + 1) * 255 / 100, 255) as u8;
                let auto = auto as u32;

                cmd.extend_from_slice(&[0x06, 0xc1, 0x00, 0x00, 0x00, 0x00]);
                cmd.extend_from_slice(&auto.to_le_bytes());
                cmd.extend_from_slice(&v.to_le_bytes());
            }
            ControlType::Sea => {
                if self.model == Model::HALO {
                    // Capture data:
                    // Data: 11c101000004 = Auto
                    // Data: 11c10100ff04 = Auto-1
                    // Data: 11c10100ce04 = Auto-50
                    // Data: 11c101323204 = Auto+50
                    // Data: 11c100646402 = 100
                    // Data: 11c100000002 = 0
                    // Data: 11c100000001 = Mode manual
                    // Data: 11c101000001 = Mode auto

                    cmd.extend_from_slice(&[0x11, 0xc1]);
                    if auto == 0 {
                        cmd.extend_from_slice(&0x00000001u32.to_le_bytes());
                        self.send(&cmd).await?;
                        cmd.clear();
                        cmd.extend_from_slice(&[0x11, 0xc1, 0x00, value as u8, value as u8, 0x02]);
                    } else {
                        cmd.extend_from_slice(&0x01000001u32.to_le_bytes());
                        self.send(&cmd).await?;
                        cmd.clear();
                        cmd.extend_from_slice(&[0x11, 0xc1, 0x01, 0x00, value as i8 as u8, 0x04]);
                    }
                } else {
                    let v: i32 = min((value + 1) * 255 / 100, 255);
                    let auto = auto as u32;

                    cmd.extend_from_slice(&[0x06, 0xc1, 0x02]);
                    cmd.extend_from_slice(&auto.to_le_bytes());
                    cmd.extend_from_slice(&v.to_le_bytes());
                }
            }
            ControlType::Rain => {
                let v = min((value + 1) * 255 / 100, 255) as u8;
                cmd.extend_from_slice(&[0x06, 0xc1, 0x04, 0, 0, 0, 0, 0, 0, 0, v]);
            }
            ControlType::SideLobeSuppression => {
                let v = min((value + 1) * 255 / 100, 255) as u8;

                cmd.extend_from_slice(&[0x06, 0xc1, 0x05, 0, 0, 0, auto, 0, 0, 0, v]);
            }
            ControlType::InterferenceRejection => {
                cmd.extend_from_slice(&[0x08, 0xc1, value as u8]);
            }
            ControlType::TargetExpansion => {
                if self.model == Model::HALO {
                    cmd.extend_from_slice(&[0x12, 0xc1, value as u8]);
                } else {
                    cmd.extend_from_slice(&[0x09, 0xc1, value as u8]);
                }
            }
            ControlType::TargetBoost => {
                cmd.extend_from_slice(&[0x0a, 0xc1, value as u8]);
            }
            ControlType::SeaState => {
                cmd.extend_from_slice(&[0x0b, 0xc1, value as u8]);
            }
            ControlType::NoTransmitStart1
            | ControlType::NoTransmitStart2
            | ControlType::NoTransmitStart3
            | ControlType::NoTransmitStart4 => {
                let sector: u8 =
                    cv.id.to_u8().unwrap() - ControlType::NoTransmitStart1.to_u8().unwrap();
                cmd.extend_from_slice(&[0x0d, 0xc1, sector, 0, 0, 0, auto]);
                todo!();
            }
            ControlType::NoTransmitEnd1
            | ControlType::NoTransmitEnd2
            | ControlType::NoTransmitEnd3
            | ControlType::NoTransmitEnd4 => {
                let sector: u8 =
                    cv.id.to_u8().unwrap() - ControlType::NoTransmitEnd1.to_u8().unwrap();
                cmd.extend_from_slice(&[0x0d, 0xc1, sector, 0, 0, 0, auto]);
                todo!();
            }
            ControlType::LocalInterferenceRejection => {
                cmd.extend_from_slice(&[0x0e, 0xc1, value as u8]);
            }
            ControlType::ScanSpeed => {
                cmd.extend_from_slice(&[0x0f, 0xc1, value as u8]);
            }
            ControlType::Mode => {
                cmd.extend_from_slice(&[0x10, 0xc1, value as u8]);
            }
            ControlType::NoiseRejection => {
                cmd.extend_from_slice(&[0x21, 0xc1, value as u8]);
            }
            ControlType::TargetSeparation => {
                cmd.extend_from_slice(&[0x22, 0xc1, value as u8]);
            }
            ControlType::Doppler => {
                cmd.extend_from_slice(&[0x23, 0xc1, value as u8]);
            }
            ControlType::DopplerSpeedThreshold => {
                let value = value as u16 * 16;
                cmd.extend_from_slice(&[0x24, 0xc1]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlType::AntennaHeight => {
                let value = value as u16 * 10;
                cmd.extend_from_slice(&[0x30, 0xc1, 0x01, 0, 0, 0]);
                cmd.extend_from_slice(&value.to_le_bytes());
                cmd.extend_from_slice(&[0, 0]);
            }
            ControlType::AccentLight => {
                cmd.extend_from_slice(&[0x31, 0xc1, value as u8]);
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlType(cv.id)),
        };

        log::info!("{}: Send command {:02X?}", self.info.key(), cmd);
        self.send(&cmd).await?;

        if self.fake_errors && cv.id == ControlType::Rain && value > 10 {
            return Self::generate_fake_error(value);
        }
        Ok(())
    }

    pub(super) async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        self.send(&REQUEST_03_REPORT).await?;
        self.send(&REQUEST_MANY2_REPORT).await?;
        self.send(&COMMAND_STAY_ON_A).await?;
        Ok(())
    }
}
