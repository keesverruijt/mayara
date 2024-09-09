use log::{debug, trace};
use std::cmp::min;
use std::io;
use tokio::net::UdpSocket;

use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlValue};
use crate::util::create_multicast_send;

pub const REQUEST_03_REPORT: [u8; 2] = [0x04, 0xc2]; // This causes the radar to report Report 3
pub const REQUEST_MANY2_REPORT: [u8; 2] = [0x01, 0xc2]; // This causes the radar to report Report 02, 03, 04, 07 and 08
pub const _REQUEST_04_REPORT: [u8; 2] = [0x02, 0xc2]; // This causes the radar to report Report 4
pub const _REQUEST_02_08_REPORT: [u8; 2] = [0x03, 0xc2]; // This causes the radar to report Report 2 and Report 8

pub struct Command {
    key: String,
    info: RadarInfo,
    sock: Option<UdpSocket>,
}

impl Command {
    pub fn new(info: RadarInfo) -> Self {
        Command {
            key: info.key(),
            info: info,
            sock: None,
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

    pub async fn set_control(&mut self, cv: &ControlValue) -> Result<(), RadarError> {
        match cv.id {
            ControlType::Gain => { 
                let v: i32 = min((cv.value.unwrap_or(0) + 1) * 255 / 100, 255);
                let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
                let cmd: [u8;11] = [ 0x06, 0xc1, 0, 0, 0, 0, auto, 0, 0, 0, v as u8];
                self.send(&cmd).await
            },
            _ =>  Err(RadarError::CannotSetControlType(cv.id))
        }
    }
}
