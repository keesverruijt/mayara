use crate::radar::{Power, RadarError};
use crate::settings::{ControlId, ControlValue, SharedControls};

use super::Command;

fn one_byte_command(cmd: &mut Vec<u8>, lead: &[u8], value: u8) {
    two_byte_command(cmd, lead, (value as u16) << 8);
}

fn two_byte_command(cmd: &mut Vec<u8>, lead: &[u8], value: u16) {
    two_value_command(cmd, lead, value, 0);
}

fn two_value_command(cmd: &mut Vec<u8>, lead: &[u8], value1: u16, value2: u16) {
    cmd.extend_from_slice(lead);
    cmd.extend_from_slice(&[0x28, 0x00]);
    cmd.extend_from_slice(&value1.to_le_bytes());
    cmd.extend_from_slice(&value2.to_le_bytes());
}

fn get_angle_value(ct: &ControlId, controls: &SharedControls) -> i16 {
    if let Some(control) = controls.get(ct) {
        if let Some(value) = control.value {
            let value = (value * 10.0) as i16;
            return value;
        }
    }

    return 0;
}

pub async fn set_control(
    command: &mut Command,
    cv: &ControlValue,
    value: f64,
    controls: &SharedControls,
) -> Result<(), RadarError> {
    let auto: u8 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
    let enabled: u8 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };
    let v = value as u8; // todo! use transform values

    let mut cmd = Vec::with_capacity(6);

    match cv.id {
        ControlId::Power => {
            let value = match Power::from_value(&cv.value).unwrap_or(Power::Standby) {
                Power::Transmit => 1,
                _ => 0,
            };
            cmd.extend_from_slice(&[0x10, 0x00, 0x28, 0x00, value, 0x00, 0x00, 0x00]);
        }

        ControlId::Range => {
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
            one_byte_command(&mut cmd, &[0x01, 0x01], index);
        }
        ControlId::Gain => {
            one_byte_command(&mut cmd, &[0x01, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x02, 0x83], v);
            }
        }
        ControlId::ColorGain => {
            one_byte_command(&mut cmd, &[0x03, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x04, 0x03], v);
            }
        }
        ControlId::Sea => {
            one_byte_command(&mut cmd, &[0x05, 0x03], auto);
            if auto == 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x06, 0x03], v);
            }
        }
        ControlId::Rain => {
            one_byte_command(&mut cmd, &[0x0b, 0x03], enabled);
            if enabled > 0 {
                command.send(&cmd).await?;
                cmd.clear();
                one_byte_command(&mut cmd, &[0x0c, 0x03], v);
            }
        }
        ControlId::TargetExpansion => {
            one_byte_command(&mut cmd, &[0x0f, 0x03], v);
        }
        ControlId::InterferenceRejection => {
            one_byte_command(&mut cmd, &[0x11, 0x03], v);
        }
        ControlId::Mode => {
            one_byte_command(&mut cmd, &[0x14, 0x03], v);
        }
        ControlId::BearingAlignment => {
            let deci_value = (value * 10.0) as i16;
            two_byte_command(&mut cmd, &[0x01, 0x04], deci_value as u16);
        }
        ControlId::MainBangSuppression => {
            one_byte_command(&mut cmd, &[0x0a, 0x04], v);
        }
        ControlId::NoTransmitStart1 => {
            let value_start: i16 = (value * 10.0) as i16;
            let value_end: i16 = get_angle_value(&ControlId::NoTransmitEnd1, controls);
            cmd = send_no_transmit_cmd(command, value_start, value_end, enabled, 0).await?;
        }
        ControlId::NoTransmitEnd1 => {
            let value_start: i16 = get_angle_value(&ControlId::NoTransmitStart1, controls);
            let value_end: i16 = (value * 10.0) as i16;
            cmd = send_no_transmit_cmd(command, value_start, value_end, enabled, 0).await?;
        }
        ControlId::NoTransmitStart2 => {
            let value_start: i16 = (value * 10.0) as i16;
            let value_end: i16 = get_angle_value(&ControlId::NoTransmitEnd2, controls);
            cmd = send_no_transmit_cmd(command, value_start, value_end, enabled, 1).await?;
        }
        ControlId::NoTransmitEnd2 => {
            let value_start: i16 = get_angle_value(&ControlId::NoTransmitStart2, controls);
            let value_end: i16 = (value * 10.0) as i16;
            cmd = send_no_transmit_cmd(command, value_start, value_end, enabled, 1).await?;
        }
        ControlId::SeaClutterCurve => {
            one_byte_command(&mut cmd, &[0x12, 0x03], v - 1);
        }
        ControlId::Doppler => {
            one_byte_command(&mut cmd, &[0x17, 0x03], v * 3); // 0x00 or 0x03
        }

        // Non-hardware settings
        _ => return Err(RadarError::CannotSetControlId(cv.id)),
    };

    log::info!("{}: Send command {:02X?}", command.info.key(), cmd);
    command.send(&cmd).await?;

    Ok(())
}

async fn send_no_transmit_cmd(
    command: &mut Command,
    value_start: i16,
    value_end: i16,
    enabled: u8,
    sector: u8,
) -> Result<Vec<u8>, RadarError> {
    let mut cmd = Vec::with_capacity(12);

    log::info!(
        "{}: send_no_transmit_cmd start={value_start} end={value_end} enabled={enabled} sector={sector}",
        command.info.key()
    );
    two_byte_command(
        &mut cmd,
        &[0x05, 0x04],
        sector as u16 + ((enabled as u16) << 8),
    );
    log::info!("{}: Send command1 {:02X?}", command.info.key(), cmd);

    command.send(&cmd).await?;
    cmd.clear();

    two_value_command(
        &mut cmd,
        &[0x03, 0x04],
        value_start as u16,
        value_end as u16,
    );
    cmd.extend_from_slice(&[sector]);

    Ok(cmd)
}
