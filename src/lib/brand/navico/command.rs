use std::cmp::{max, min};

use async_trait::async_trait;
use tokio::net::UdpSocket;

use crate::brand::CommandSender;
use crate::network::create_multicast_send;
use crate::radar::{Power, RadarError, RadarInfo};
use crate::settings::{ControlId, ControlValue, SharedControls};

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
    pub fn new(fake_errors: bool, info: RadarInfo, model: Model) -> Self {
        Command {
            key: info.key(),
            info,
            model,
            sock: None,
            fake_errors,
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

    fn scale_100_to_byte(a: f64) -> u8 {
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

    fn generate_fake_error(v: i32) -> Result<(), RadarError> {
        match v {
            11 => Err(RadarError::CannotSetControlId(ControlId::Rain)),
            12 => Err(RadarError::CannotSetControlId(ControlId::Power)),
            _ => Err(RadarError::NoSuchRadar(1)),
        }
    }

    fn get_angle_value(ct: &ControlId, controls: &SharedControls) -> i16 {
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

    pub(super) async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        self.send(&REQUEST_03_REPORT).await?;
        self.send(&REQUEST_MANY2_REPORT).await?;
        self.send(&COMMAND_STAY_ON_A).await?;
        Ok(())
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &SharedControls,
    ) -> Result<(), RadarError> {
        log::debug!("set_control({:?},...)", cv);

        let value = cv.as_f32()?;

        let deci_value = (value * 10.0) as i32;
        let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        let mut cmd = Vec::with_capacity(6);

        match cv.id {
            ControlId::Power => {
                let value = match Power::from_value(&cv.value).unwrap_or(Power::Standby) {
                    Power::Transmit => 1,
                    _ => 0,
                };

                cmd.extend_from_slice(&[0x00, 0xc1, 0x01]);
                self.send(&cmd).await?;
                cmd.clear();
                cmd.extend_from_slice(&[0x01, 0xc1, value]);
            }

            ControlId::Range => {
                let decimeters: i32 = deci_value;
                log::trace!("range {value} -> {decimeters}");

                cmd.extend_from_slice(&[0x03, 0xc1]);
                cmd.extend_from_slice(&decimeters.to_le_bytes());
            }
            ControlId::BearingAlignment => {
                let value: i16 = Self::mod_deci_degrees(deci_value) as i16;

                cmd.extend_from_slice(&[0x05, 0xc1]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlId::Gain => {
                let v = Self::scale_100_to_byte(value);
                let auto = auto as u32;

                cmd.extend_from_slice(&[0x06, 0xc1, 0x00, 0x00, 0x00, 0x00]);
                cmd.extend_from_slice(&auto.to_le_bytes());
                cmd.extend_from_slice(&v.to_le_bytes());
            }
            ControlId::Sea => {
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
            ControlId::Rain => {
                let v = Self::scale_100_to_byte(value);
                cmd.extend_from_slice(&[0x06, 0xc1, 0x04, 0, 0, 0, 0, 0, 0, 0, v]);
            }
            ControlId::SideLobeSuppression => {
                let v = Self::scale_100_to_byte(value);

                cmd.extend_from_slice(&[0x06, 0xc1, 0x05, 0, 0, 0, auto, 0, 0, 0, v]);
            }
            ControlId::InterferenceRejection => {
                cmd.extend_from_slice(&[0x08, 0xc1, value as u8]);
            }
            ControlId::TargetExpansion => {
                if self.model == Model::HALO {
                    cmd.extend_from_slice(&[0x12, 0xc1, value as u8]);
                } else {
                    cmd.extend_from_slice(&[0x09, 0xc1, value as u8]);
                }
            }
            ControlId::TargetBoost => {
                cmd.extend_from_slice(&[0x0a, 0xc1, value as u8]);
            }
            ControlId::SeaState => {
                cmd.extend_from_slice(&[0x0b, 0xc1, value as u8]);
            }
            ControlId::NoTransmitStart1 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlId::NoTransmitEnd1, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 0)
                    .await?;
            }
            ControlId::NoTransmitStart2 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlId::NoTransmitEnd2, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 1)
                    .await?;
            }
            ControlId::NoTransmitStart3 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlId::NoTransmitEnd3, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 2)
                    .await?;
            }
            ControlId::NoTransmitStart4 => {
                let value_start: i16 = Self::mod_deci_degrees(deci_value) as i16;
                let value_end: i16 = Self::get_angle_value(&ControlId::NoTransmitEnd4, controls);
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 3)
                    .await?;
            }
            ControlId::NoTransmitEnd1 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlId::NoTransmitStart1, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 0)
                    .await?;
            }
            ControlId::NoTransmitEnd2 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlId::NoTransmitStart2, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 1)
                    .await?;
            }
            ControlId::NoTransmitEnd3 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlId::NoTransmitStart3, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 2)
                    .await?;
            }
            ControlId::NoTransmitEnd4 => {
                let value_start: i16 =
                    Self::get_angle_value(&ControlId::NoTransmitStart4, controls);
                let value_end: i16 = Self::mod_deci_degrees(deci_value) as i16;
                cmd = self
                    .send_no_transmit_cmd(value_start, value_end, enabled, 3)
                    .await?;
            }
            ControlId::LocalInterferenceRejection => {
                cmd.extend_from_slice(&[0x0e, 0xc1, value as u8]);
            }
            ControlId::ScanSpeed => {
                cmd.extend_from_slice(&[0x0f, 0xc1, value as u8]);
            }
            ControlId::Mode => {
                cmd.extend_from_slice(&[0x10, 0xc1, value as u8]);
            }
            ControlId::NoiseRejection => {
                cmd.extend_from_slice(&[0x21, 0xc1, value as u8]);
            }
            ControlId::TargetSeparation => {
                cmd.extend_from_slice(&[0x22, 0xc1, value as u8]);
            }
            ControlId::Doppler => {
                cmd.extend_from_slice(&[0x23, 0xc1, value as u8]);
            }
            ControlId::DopplerSpeedThreshold => {
                let value = (value * 100.0) as u16;
                let value = max(0, min(1594, value));
                cmd.extend_from_slice(&[0x24, 0xc1]);
                cmd.extend_from_slice(&value.to_le_bytes());
            }
            ControlId::AntennaHeight => {
                let value = deci_value as u16;
                cmd.extend_from_slice(&[0x30, 0xc1, 0x01, 0, 0, 0]);
                cmd.extend_from_slice(&value.to_le_bytes());
                cmd.extend_from_slice(&[0, 0]);
            }
            ControlId::AccentLight => {
                cmd.extend_from_slice(&[0x31, 0xc1, value as u8]);
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlId(cv.id)),
        };

        log::debug!("{}: Send command {:02X?}", self.info.key(), cmd);
        self.send(&cmd).await?;

        if self.fake_errors && cv.id == ControlId::Rain && value > 10. {
            return Self::generate_fake_error(value as i32);
        }
        Ok(())
    }
}
