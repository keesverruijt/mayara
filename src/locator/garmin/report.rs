use log::{debug, trace, warn};

use crate::util::c_string;

pub fn process(report: &[u8]) {
    if report.len() < size_of::<u32>() * 2 + size_of::<u8>() {
        return;
    }

    let (packet_type, data) = report.split_at(std::mem::size_of::<u32>());
    let (len, data) = data.split_at(std::mem::size_of::<u32>());
    let packet_type = u32::from_le_bytes(packet_type.try_into().unwrap());
    let len = u32::from_le_bytes(len.try_into().unwrap());

    trace!(
        "Garmin message type {} len {} data {:?}",
        packet_type,
        len,
        data
    );

    if data.len() != len as usize {
        warn!(
            "Received incomplete message of len {} expected {}",
            data.len(),
            len
        );
        return;
    }
    let value: u32 = match len {
        1 => data[0] as u32,
        2 => {
            let ins_bytes = <[u8; 2]>::try_from(&data[0..2]).unwrap();
            u16::from_le_bytes(ins_bytes) as u32
        }
        4 => {
            let ins_bytes = <[u8; 4]>::try_from(&data[0..4]).unwrap();
            u32::from_le_bytes(ins_bytes)
        }
        _ => 0,
    };

    match packet_type {
        0x0916 => {
            debug!("Scan speed {}", value);
        }
        0x0919 => {
            debug!("Transmit state {}", value);
        }
        0x091e => {
            debug!("Range {} m", value);
        }
        //
        // Garmin sends range in three separate packets, in the order 0x924, 0x925, 0x91d every
        // two seconds.
        // Auto High: 0x924 = 2, 0x925 = gain, 0x91d = 1
        // Auto Low:  0x924 = 2, 0x925 = gain, 0x91d = 0
        // Manual:    0x924 = 0, 0x925 = gain, 0x91d = 0 (could be last one used?)
        0x0924 => {
            debug!("Autogain {}", value);
        }
        0x0925 => {
            debug!("Gain {}", value);
        }
        0x091d => {
            debug!("Autogain level {}", value);
        }
        0x0930 => {
            debug!("Bearing alignment {}", value as i32 as f32 / 32.0);
        }
        0x0932 => {
            debug!("Crosstalk rejection {}", value);
        }
        0x0933 => {
            debug!("Rain clutter mode {}", value);
        }
        0x0934 => {
            debug!("Rain clutter level {}", value);
        }
        0x0939 => {
            debug!("Sea clutter mode {}", value);
        }
        0x093a => {
            debug!("Sea clutter level {}", value);
        }
        0x093b => {
            debug!("Sea clutter auto level {}", value);
        }
        0x093f => {
            debug!("No transmit zone mode {}", value);
        }
        0x0940 => {
            debug!("No transmit zone start {}", value as i32 as f32 / 32.0);
        }
        0x0941 => {
            debug!("No transmit zone end {}", value as i32 as f32 / 32.0);
        }
        0x0942 => {
            debug!("Time idle mode {}", value);
        }
        0x0943 => {
            debug!("Time idle time {}", value);
        }
        0x0944 => {
            debug!("Time idle run time {}", value);
        }
        0x0992 => {
            debug!("Scanner status {}", value);
        }
        0x0993 => {
            debug!("Scanner status change in {} ms", value);
        }
        0x099b => {
            let info: [u8; 64] = <[u8; 64]>::try_from(&data[16..16 + 64]).unwrap();
            let info = c_string(&info).unwrap_or("?");

            debug!("Scanner message \"{}\"", info);
        }

        _ => {}
    }
}
