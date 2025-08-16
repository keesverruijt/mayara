use std::cmp::{max, min};

use log::{debug, trace};
use tokio::net::UdpSocket;

use crate::network::create_multicast_send;
use crate::radar::range::Ranges;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlValue, Controls};
use crate::Session;

use super::Model;

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

    pub fn set_ranges(&mut self, ranges: Ranges) {
        self.info.ranges = ranges;
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

    fn generate_fake_error(v: i32) -> Result<(), RadarError> {
        match v {
            11 => Err(RadarError::CannotSetControlType(ControlType::Rain)),
            12 => Err(RadarError::CannotSetControlType(ControlType::Status)),
            _ => Err(RadarError::NoSuchRadar("FakeRadarKey".to_string())),
        }
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

    fn standard_command(&self, cmd: &mut Vec<u8>, lead: &[u8], value: u8) {
        cmd.extend_from_slice(lead);
        cmd.extend_from_slice(&[
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, value, 0x00, 0x00, 0x00,
        ]);
    }

    fn on_off_command(&self, cmd: &mut Vec<u8>, lead: &[u8], on_off: u8) {
        cmd.extend_from_slice(lead);
        cmd.extend_from_slice(&[
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            on_off, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
    }

    pub async fn set_control(
        &mut self,
        cv: &ControlValue,
        controls: &Controls,
    ) -> Result<(), RadarError> {
        let value = cv
            .value
            .parse::<f32>()
            .map_err(|_| RadarError::MissingValue(cv.id))?;
        let deci_value = (value * 10.0) as i32;
        let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
        let v = Self::scale_100_to_byte(value); // todo! use transform values

        let mut cmd = Vec::with_capacity(6);

        match cv.id {
            ControlType::Status => {
                cmd.extend_from_slice(&[0x01, 0x80, 0x01, 0x00, value as u8 - 1, 0x00, 0x00, 0x00]);
            }

            ControlType::Range => {
                let value = value as i32;
                let ranges = &self.info.ranges;
                let index = if value < ranges.len() as i32 {
                    value as u8
                } else {
                    let mut i = 0;
                    for r in ranges.all.iter() {
                        if r.distance() >= value {
                            break;
                        }
                        i += 1;
                    }
                    i
                };
                log::trace!("range {value} -> {index}");
                cmd.extend_from_slice(&[
                    0x01, 0x81, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                    index, // Range at offset 8 (0 - 1/8, 1 - 1/4, 2 - 1/2, 3 - 3/4, 4 - 1, 5 - 1.5, 6 - 3...)
                    0x00, 0x00, 0x00,
                ]);
            }
            ControlType::BearingAlignment => {
                cmd.extend_from_slice(&[0x07, 0x82, 0x01, 0x00]);
                // to be consistent with the local bearing alignment of the pi
                // this bearing alignment works opposite to the one an a Lowrance display
                cmd.extend_from_slice(&(deci_value as u32).to_le_bytes());
            }

            ControlType::Gain => {
                self.on_off_command(&mut cmd, &[0x01, 0x83], auto);
                if auto == 0 {
                    self.send(&cmd).await?;
                    cmd.clear();
                    self.standard_command(&mut cmd, &[0x01, 0x83], v);
                }
            }
            ControlType::Sea => {
                self.on_off_command(&mut cmd, &[0x02, 0x83], auto);
                if auto == 0 {
                    self.send(&cmd).await?;
                    cmd.clear();
                    self.standard_command(&mut cmd, &[0x02, 0x83], v);
                }
            }
            ControlType::Rain => {
                self.on_off_command(&mut cmd, &[0x03, 0x83], auto);
                if auto == 0 {
                    self.send(&cmd).await?;
                    cmd.clear();
                    self.standard_command(&mut cmd, &[0x03, 0x83], v);
                }
            }
            ControlType::Ftc => {
                let on_off = 1 - auto; // Ftc is really an on/off switch, so invert auto
                self.on_off_command(&mut cmd, &[0x04, 0x83], on_off);
                if on_off == 1 {
                    self.send(&cmd).await?;
                    cmd.clear();
                    self.standard_command(&mut cmd, &[0x04, 0x83], v);
                }
            }
            ControlType::MainBangSuppression => {
                let on_off = 1 - auto; // Ftc is really an on/off switch, so invert auto
                self.standard_command(&mut cmd, &[0x01, 0x82], on_off);
            }
            ControlType::DisplayTiming => {
                cmd.extend_from_slice(&[
                    0x02, 0x82, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00,
                    v, // Display timing value at offset 8
                    0x00, 0x00, 0x00,
                ]);
            }
            ControlType::InterferenceRejection => {
                cmd.extend_from_slice(&[
                    0x07, 0x83, 0x01, 0x00,
                    v, // Interference rejection at offset 4, 0 - off, 1 - normal, 2 - high
                    0x00, 0x00, 0x00,
                ]);
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlType(cv.id)),
        };

        log::info!("{}: Send command {:02X?}", self.info.key(), cmd);
        self.send(&cmd).await?;

        if self.fake_errors && cv.id == ControlType::Rain && value > 10. {
            return Self::generate_fake_error(value as i32);
        }
        Ok(())
    }

    pub(super) async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        Ok(())
    }
}
