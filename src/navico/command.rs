use log::{debug, trace};
use std::io;
use std::sync::Arc;
use tokio::net::UdpSocket;

use crate::radar::RadarLocationInfo;
use crate::util::create_multicast_send;

use super::NavicoSettings;

pub const REQUEST_03_REPORT: [u8; 2] = [0x04, 0xc2]; // This causes the radar to report Report 3
pub const REQUEST_MANY2_REPORT: [u8; 2] = [0x01, 0xc2]; // This causes the radar to report Report 02, 03, 04, 07 and 08
pub const _REQUEST_04_REPORT: [u8; 2] = [0x02, 0xc2]; // This causes the radar to report Report 4
pub const _REQUEST_02_08_REPORT: [u8; 2] = [0x03, 0xc2]; // This causes the radar to report Report 2 and Report 8

pub struct Command {
    key: String,
    info: RadarLocationInfo,
    sock: Option<UdpSocket>,
    settings: Arc<NavicoSettings>,
}

impl Command {
    pub fn new(info: RadarLocationInfo, settings: Arc<NavicoSettings>) -> Self {
        Command {
            key: info.key(),
            info: info,
            sock: None,
            settings,
        }
    }

    async fn start_socket(&mut self) -> io::Result<()> {
        match create_multicast_send(&self.info.send_command_addr, &self.info.nic_addr) {
            Ok(sock) => {
                debug!(
                    "{} {} via {}: sending commands",
                    self.key, &self.info.send_command_addr, &self.info.nic_addr
                );
                self.sock = Some(sock);
                debug!("{}: Settings {:?}", self.key, self.settings);

                Ok(())
            }
            Err(e) => {
                debug!(
                    "{} {} via {}: create multicast failed: {}",
                    self.key, &self.info.send_command_addr, &self.info.nic_addr, e
                );
                Err(e)
            }
        }
    }

    pub async fn send(&mut self, message: &[u8]) -> io::Result<()> {
        if self.sock.is_none() {
            self.start_socket().await?;
        }
        if let Some(sock) = &self.sock {
            sock.send(message).await?;
            trace!("{}: sent {:02X?}", self.key, message);
        }

        Ok(())
    }
}
