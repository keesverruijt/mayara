use std::str::FromStr;

use crate::radar::{RadarError, Status};
use crate::settings::{ControlType, ControlValue, SharedControls};

use super::Command;

fn standard_command(cmd: &mut Vec<u8>, lead: &[u8], value: u8) {
    cmd.extend_from_slice(lead);
    cmd.extend_from_slice(&[
        0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, value, 0x00, 0x00, 0x00,
    ]);
}

fn on_off_command(cmd: &mut Vec<u8>, lead: &[u8], on_off: u8) {
    cmd.extend_from_slice(lead);
    cmd.extend_from_slice(&[
        0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, on_off,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ]);
}

pub async fn set_control(
    command: &mut Command,
    cv: &ControlValue,
    value: f32,
    _controls: &SharedControls, // Not used now, but useful if controls depend on other controls
) -> Result<(), RadarError> {
    let deci_value = (value * 10.0) as i32;
    let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
    let _enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
    let v = Command::scale_100_to_byte(value); // todo! use transform values

    let mut cmd = Vec::with_capacity(6);

    match cv.id {
        ControlType::Power => {
            let value = match Status::from_str(&cv.value).unwrap_or(Status::Standby) {
                Status::Transmit => 1,
                _ => 0,
            };
            cmd.extend_from_slice(&[0x01, 0x80, 0x01, 0x00, value, 0x00, 0x00, 0x00]);
        }

        ControlType::Range => {
            let value = value as i32;
            let ranges = &command.info.ranges;
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
            on_off_command(&mut cmd, &[0x01, 0x83], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x01, 0x83], v);
            }
        }
        ControlType::Sea => {
            on_off_command(&mut cmd, &[0x02, 0x83], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x02, 0x83], v);
            }
        }
        ControlType::Rain => {
            on_off_command(&mut cmd, &[0x03, 0x83], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x03, 0x83], v);
            }
        }
        ControlType::Ftc => {
            let on_off = 1 - auto; // Ftc is really an on/off switch, so invert auto
            on_off_command(&mut cmd, &[0x04, 0x83], on_off);
            if on_off == 1 {
                command.send(&cmd).await?;
                cmd.clear();
                standard_command(&mut cmd, &[0x04, 0x83], v);
            }
        }
        ControlType::MainBangSuppression => {
            let on_off = 1 - auto; // Ftc is really an on/off switch, so invert auto
            standard_command(&mut cmd, &[0x01, 0x82], on_off);
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

    log::info!("{}: Send command {:02X?}", command.info.key(), cmd);
    command.send(&cmd).await?;

    Ok(())
}
