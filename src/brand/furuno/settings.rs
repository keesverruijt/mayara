use std::collections::HashMap;

use crate::{
    radar::RadarInfo,
    settings::{Control, ControlType, SharedControls},
};

use super::{RadarModel, FURUNO_RADAR_RANGES};

pub fn new() -> SharedControls {
    let mut controls = HashMap::new();

    controls.insert(
        ControlType::UserName,
        Control::new_string(ControlType::UserName).read_only(false),
    );

    let max_value = 120. * 1852.;
    let mut range_control = Control::new_numeric(ControlType::Range, 0., max_value).unit("m");
    range_control.set_valid_values(FURUNO_RADAR_RANGES.into());
    controls.insert(ControlType::Range, range_control);

    controls.insert(
        ControlType::OperatingHours,
        Control::new_numeric(ControlType::OperatingHours, 0., 999999.)
            .read_only(true)
            .unit("h"),
    );

    controls.insert(
        ControlType::RotationSpeed,
        Control::new_numeric(ControlType::RotationSpeed, 0., 99.)
            .wire_scale_factor(990., true)
            .read_only(true)
            .unit("RPM"),
    );

    let mut control = Control::new_list(
        ControlType::Status,
        &["WarmingUp", "Standby", "Transmit", "NoConnection"],
    )
    .send_always();
    control.set_valid_values([1, 2].to_vec());
    controls.insert(ControlType::Status, control);

    SharedControls::new(controls)
}

pub fn update_when_model_known(info: &mut RadarInfo, model: RadarModel, version: &str) {
    let model_name = model.to_str();
    log::debug!("update_when_model_known: {}", model_name);
    info.controls.set_model_name(model_name.to_string());

    let mut control = Control::new_string(ControlType::SerialNumber);
    if let Some(serial_number) = info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    info.controls.insert(ControlType::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Furuno ... ' name.
    if info.controls.user_name() == info.key() {
        let mut user_name = model_name.to_string();
        if info.serial_no.is_some() {
            let serial = info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial);
        }
        info.controls.set_user_name(user_name);
    }

    // TODO: Add controls based on reverse engineered capability table

    info.controls.insert(
        ControlType::FirmwareVersion,
        Control::new_string(ControlType::FirmwareVersion),
    );
    info.controls
        .set_string(&ControlType::FirmwareVersion, version.to_string())
        .expect("FirmwareVersion");

    info.controls.insert(
        ControlType::NoTransmitStart1,
        Control::new_numeric(ControlType::NoTransmitStart1, -180., 180.)
            .unit("Deg")
            .wire_offset(-1.),
    );
    info.controls.insert(
        ControlType::NoTransmitEnd1,
        Control::new_numeric(ControlType::NoTransmitEnd1, -180., 180.)
            .unit("Deg")
            .wire_offset(-1.),
    );

    info.controls.insert(
        ControlType::NoTransmitStart2,
        Control::new_numeric(ControlType::NoTransmitStart2, -180., 180.)
            .unit("Deg")
            .wire_offset(-1.),
    );
    info.controls.insert(
        ControlType::NoTransmitEnd2,
        Control::new_numeric(ControlType::NoTransmitEnd2, -180., 180.)
            .unit("Deg")
            .wire_offset(-1.),
    );
}
