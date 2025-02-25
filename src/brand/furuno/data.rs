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

use crate::locator::{Locator, LocatorId};
use crate::network::create_udp_multicast_listen;
use crate::protos::RadarMessage::radar_message::Spoke;
use crate::protos::RadarMessage::RadarMessage;
use crate::radar::*;
use crate::util::PrintableSpoke;

use super::{FURUNO_SPOKES, FURUNO_SPOKE_LEN};

const BYTE_LOOKUP_LENGTH: usize = (u8::MAX as usize) + 1;

pub struct FurunoDataReceiver {
    key: String,
    statistics: Statistics,
    info: RadarInfo,
    sock: Option<UdpSocket>,
    rx: tokio::sync::mpsc::Receiver<i32>,
    doppler: DopplerMode,
    // pixel_to_blob: [[u8; BYTE_LOOKUP_LENGTH]; LOOKUP_SPOKE_LENGTH],
    replay: bool,
    trails: TrailBuffer,
    previous_range: u32,
}

impl FurunoDataReceiver {
    pub fn new(info: RadarInfo, rx: Receiver<i32>, replay: bool) -> FurunoDataReceiver {
        let key = info.key();

        // let pixel_to_blob = Self::pixel_to_blob(&info.legend);
        let trails = TrailBuffer::new(info.legend.clone(), FURUNO_SPOKES, FURUNO_SPOKE_LEN);

        FurunoDataReceiver {
            key,
            statistics: Statistics { broken_packets: 0 },
            info: info,
            sock: None,
            rx,
            doppler: DopplerMode::None,
            //pixel_to_blob,
            replay,
            trails,
            previous_range: 0,
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

        loop {
            tokio::select! { biased;
                _ = subsys.on_shutdown_requested() => {
                    return Err(RadarError::Shutdown);
                },
                _r = self.rx.recv() => {
                  // self.handle_data_update(r);
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
        log::info!("Received spoke {:?}", data);
    }
}
