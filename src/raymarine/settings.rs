use std::collections::HashMap;

use crate::{
    radar::RadarInfo,
    settings::{Control, ControlType, Controls, HAS_AUTO_NOT_ADJUSTABLE},
};

pub fn new(replay: bool) -> Controls {
    let mut controls = HashMap::new();

    controls.insert(
        ControlType::UserName,
        Control::new_string(ControlType::UserName).read_only(false),
    );

    controls.insert(
        ControlType::AntennaHeight,
        Control::new_numeric(ControlType::AntennaHeight, 0., 9900.).unit("cm"),
    );
    controls.insert(
        ControlType::BearingAlignment,
        Control::new_numeric(ControlType::BearingAlignment, -180., 180.)
            .unit("Deg")
            .wire_scale_factor(1800., true)
            .wire_offset(-1.),
    );
    controls.insert(
        ControlType::Gain,
        Control::new_auto(ControlType::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(255., false),
    );
    controls.insert(
        ControlType::Sea,
        Control::new_auto(ControlType::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(255., false),
    );
    controls.insert(
        ControlType::Rain,
        Control::new_numeric(ControlType::Rain, 0., 100.).wire_scale_factor(2., false),
    );

    controls.insert(
        ControlType::Ftc,
        Control::new_numeric(ControlType::Ftc, 0., 100.).wire_scale_factor(2., false),
    );
    controls.insert(
        ControlType::InterferenceRejection,
        Control::new_list(
            ControlType::InterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );

    let mut control = Control::new_list(
        ControlType::Status,
        &["Off", "Standby", "Transmit", "", "", "SpinningUp"],
    )
    .send_always();
    control.set_valid_values([1, 2].to_vec());

    controls.insert(ControlType::Status, control);

    controls.insert(
        ControlType::SideLobeSuppression,
        Control::new_auto(
            ControlType::SideLobeSuppression,
            0.,
            100.,
            HAS_AUTO_NOT_ADJUSTABLE,
        )
        .wire_scale_factor(255., false),
    );

    Controls::new_base(controls, replay)
}

pub fn update_when_model_known(controls: &mut Controls, radar_info: &RadarInfo) {
    controls.set_model_name(radar_info.which.as_deref().unwrap_or("?").to_string());

    let mut control = Control::new_string(ControlType::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlType::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Furuno ... ' name.
    if radar_info.user_name() == radar_info.key() {
        let mut user_name = "Raymarine".to_string();
        if radar_info.serial_no.is_some() {
            let mut serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial.split_off(7));
        }
        if radar_info.which.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.which.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    let max_value = 48. * 1852.;
    let mut range_control = Control::new_numeric(ControlType::Range, 0., max_value)
        .unit("m")
        .wire_scale_factor(10. * max_value, false); // Radar sends and receives in decimeters
    if let Some(range_detection) = &radar_info.range_detection {
        if range_detection.complete {
            range_control.set_valid_values(range_detection.ranges.clone());
        }
    };
    controls.insert(ControlType::Range, range_control);
    controls.insert(
        ControlType::MainBangSuppression,
        Control::new_list(ControlType::MainBangSuppression, &["Off", "On"]),
    );
    controls.insert(
        ControlType::TargetBoost,
        Control::new_list(ControlType::TargetBoost, &["Off", "Medium", "High"]),
    );
}
