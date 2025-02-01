use std::ptr::null_mut;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::NetworkManagement::IpHelper::NotifyAddrChange;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
use windows::Win32::System::IO::OVERLAPPED;

use crate::radar::RadarError;

pub async fn wait_for_ip_addr_change(cancel_token: CancellationToken) -> Result<(), RadarError> {
    let (tx, mut rx) = watch::channel(false);

    let handle = unsafe {
        // Create a manual-reset event
        let event = CreateEventW(None, true, false, None)
            .map_err(|_| RadarError::Io(std::io::Error::last_os_error()))?;
        if event.is_invalid() {
            return Err(RadarError::Io(std::io::Error::last_os_error()));
        }

        // Prepare an OVERLAPPED structure with the event handle
        let mut overlapped = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };

        // Register for address change notifications
        let notify_result = NotifyAddrChange(null_mut(), &mut overlapped);
        if notify_result != 0 {
            CloseHandle(event);
            return Err(RadarError::Io(std::io::Error::last_os_error()));
        }

        // Spawn a blocking task to wait for the event
        let handle = tokio::task::spawn_blocking(move || {
            let result = WaitForSingleObject(event, INFINITE);
            let _ = tx.send(result.0 == 0);
            CloseHandle(event);
        });
        handle
    };

    // Wait asynchronously for a change or cancellation
    tokio::select! {
        _ = cancel_token.cancelled() => {
            // Cancel the operation
            handle.abort();
            return Err(RadarError::Shutdown);
        }
        change_result = rx.changed() => {
            if change_result.is_ok() && *rx.borrow() {
                return Ok(());
            } else {
                return Err(RadarError::EnumerationFailed);
            }
        }
    }
}
