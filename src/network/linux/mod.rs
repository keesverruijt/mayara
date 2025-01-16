use crate::radar::RadarError;

use netlink_packet_core::{NetlinkMessage, NetlinkPayload};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_sys::{constants::NETLINK_ROUTE, Socket, SocketAddr};
use rtnetlink::constants::RTMGRP_IPV4_IFADDR;
use tokio::io::unix::AsyncFd;
use tokio_util::sync::CancellationToken;

/// Waits asynchronously for an IPv4 address change on Linux.
/// Completes when an address change is detected or the cancellation token is triggered.
pub async fn wait_for_ip_addr_change(cancel_token: CancellationToken) -> Result<(), RadarError> {
    // Create a netlink socket for routing events
    let mut socket = Socket::new(NETLINK_ROUTE).map_err(|e| RadarError::Io(e))?;

    let sockaddr_nl = SocketAddr::new(0, RTMGRP_IPV4_IFADDR);
    socket.bind(&sockaddr_nl).map_err(|e| RadarError::Io(e))?;

    // Make the socket asynchronous
    let async_socket = AsyncFd::new(socket)?;

    log::trace!("Waiting for IP address change");
    loop {
        tokio::select! {
            // Check for cancellation
            _ = cancel_token.cancelled() => {
                log::trace!("Shutdown requested");
                return Ok(());
            }

            // Wait for messages on the socket
            result = async_socket.readable() => {
                let _ = result?; // Ensure readiness was successful

                // Process the available data
                let mut buf = vec![0; 4096];
                match async_socket.get_ref().recv(&mut buf, 0) {
                    Ok(len) => {
                        buf.truncate(len);
                        log::trace!("Received {} bytes", len);

                        // Parse the netlink message
                        if let Ok(message) = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&buf) {
                            match message.payload {
                                NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewAddress(_)) => {
                                    // Detected a new IP address
                                    log::trace!("Detected new IP address");
                                    return Ok(());
                                }
                                NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelAddress(_)) => {
                                    // Detected a removed IP address
                                    log::trace!("Detected removed IP address");
                                    return Ok(());
                                }
                                _ => {
                                    // Ignore other messages
                                    log::trace!("Ignoring message: {:?}", message);
                                }
                            }
                        } else {
                            log::trace!("Failed to parse message");
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue; // No data to read, loop again
                    }
                    Err(e) => {
                        return Err(RadarError::Io(e));
                    }
                }
            }
        }
    }
}
