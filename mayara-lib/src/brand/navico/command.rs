use std::cmp::{max, min};

use tokio::net::UdpSocket;

use crate::network::create_multicast_send;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlValue, SharedControls};
use crate::Session;

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
    pub fn new(session: Session, info: RadarInfo, model: Model) -> Self {
        Command {
            key: info.key(),
            info,
            model,
            sock: None,
            fake_errors: session.read().unwrap().args.fake_errors,
        }
    }

    async fn start_socket(&mut self) -> Result<(), RadarError> {
        match create_multicast_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                log::debug!(
                    "{} {} via {}: sending commands",
                    self.key,
                    &self.info.send_command_addr,
                    &self.info.nic_addr
                );
                self.sock = Some(sock);

                Ok(())
            }
            Err(e) => {
                log::debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key,
                    &self.info.send_command_addr,
                    &self.info.nic_addr,
                    e
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
            log::trace!("{}: sent {:02X?}", self.key, message);
        }

        Ok(())
    }

    fn scale_100_to_byte(a: f32) -> u8 {
        // Map range 0..100 to 0..255
        let mut r = a * 255.0 / 100.0;
        if r > 255.0 {
            r = 255.0;
        } else if r < 0.0 {
            r = 0.0;
        }
        r as u8
    }

    fn mod_deci_degrees(a: i32) -> i32 {
        (a + 7200) % 3600
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
            min(max(i, 50), 96 * 1852)
        }
    }

    fn generate_fake_error(v: i32) -> Result<(), RadarError> {
        match v {
            11 => Err(RadarError::CannotSetControlType(ControlType::Rain)),
            12 => Err(RadarError::CannotSetControlType(ControlType::Status)),
            _ => Err(RadarError::NoSuchRadar("FakeRadarKey".to_string())),
        }
    }

    fn get_angle_value(ct: &ControlType, controls: &SharedControls) -> i16 {
        if let Some(control) = controls.get(ct) {
            if let Some(value) = control.value {
                let value = (value * 10.0) as i32;
                return Self::mod_deci_degrees(value) as i16;
            }
        }
        return 0;
    }

    async fn send_no_transmit_cmd(
        &mut self,
        value_start: i16,
        value_end: i16,
        enabled: u8,
        sector: u8,
    ) -> Result<Vec<u8>, RadarError> {
        let mut cmd = Vec::with_capacity(12);

        cmd.extend_from_slice(&[0x0d, 0xc1, sector, 0, 0, 0, enabled]);
        self.send(&cmd).await?;
        cmd.clear();
        cmd.extend_from_slice(&[0xc0, 0xc1, sector, 0, 0, 0, enabled]);
        cmd.extend_from_slice(&value_start.to_le_bytes());
        cmd.extend_from_slice(&value_end.to_le_bytes());

        Ok(cmd)
    }

    pub async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        let value = cv
            .value
            .parse::<f32>()
            .map_err(|_| RadarError::MissingValue(cv.id))?;
        let deci_value = (value * 10.0) as i32;
        let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        let mut cmd = Vec::with_capacity(6);

        match cv.id {
            ControlType::Status => {
                cmd.extend_from_slice(&[0x00, 0xc1, 0x01]);
                self.send(&cmd).await?;
                cmd.clear();
                cmd.extend_from_slice(&[0x01, 0xc1, value as u8 - 1]);
            }

            ControlType::Range => {
                let decimeters: i32 = self.valid_range(deci_value / 10) * 10; //TODO
                log::trace!("range {value} -> {decimeters}");

                cmd.extend_from_slice(&[0x03, 0xc1]);
                cmd.extend_from_slice(&decimeters.to_le_bytes());
            }
            ControlType::BearingAlignment => {
                let value: i16 = Self::mod_deci_degrees(deci_value) as i16;

                cmd.extend_from_slice(&[0x05, 0xc1]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlType::Gain => {
                let v = Self::scale_100_to_byte(value);
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
                    let v: u32 = Self::scale_100_to_byte(value) as u32;
                    let auto = auto as u32;

                    cmd.extend_from_slice(&[0x06, 0xc1, 0x02]);
                    cmd.extend_from_slice(&auto.to_be_bytes());
                    cmd.extend_from_slice(&v.to_be_bytes());
                }
            }
            ControlType::Rain => {
                let v = Self::scale_100_to_byte(value);
                cmd.extend_from_slice(&[0x06, 0xc1, 0x04, 0, 0, 0, 0, 0, 0, 0, v]);
            }
            ControlType::SideLobeSuppression => {
                let v = Self::scale_100_to_byte(value);

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
            ControlType::NoTransmitStart1 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlType::NoTransmitEnd1, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 0)
                    .await?;
            }
            ControlType::NoTransmitStart2 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlType::NoTransmitEnd2, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 1)
                    .await?;
            }
            ControlType::NoTransmitStart3 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlType::NoTransmitEnd3, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 2)
                    .await?;
            }
            ControlType::NoTransmitStart4 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlType::NoTransmitEnd4, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 3)
                    .await?;
            }
            ControlType::NoTransmitEnd1 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlType::NoTransmitStart1, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 0)
                    .await?;
            }
            ControlType::NoTransmitEnd2 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlType::NoTransmitStart2, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 1)
                    .await?;
            }
            ControlType::NoTransmitEnd3 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlType::NoTransmitStart3, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 2)
                    .await?;
            }
            ControlType::NoTransmitEnd4 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlType::NoTransmitStart4, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 3)
                    .await?;
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
                let value = deci_value as u16;
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

        log::debug!("{}: Send command {:02X?}", self.info.key(), cmd);
        self.send(&cmd).await?;

        if self.fake_errors && cv.id == ControlType::Rain && value > 10. {
            return Self::generate_fake_error(value as i32);
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
