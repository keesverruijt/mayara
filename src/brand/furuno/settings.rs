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

    let max_value = 48. * 1852.;
    let range_control = Control::new_numeric(ControlType::Range, 0., max_value).unit("m");
    controls.insert(ControlType::Range, range_control);

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
        Control::new_auto(ControlType::Rain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(255., false),
    );
    controls.insert(
        ControlType::TargetBoost,
        Control::new_list(ControlType::TargetBoost, &["Off", "Low", "High"]),
    );

    controls.insert(
        ControlType::OperatingHours,
        Control::new_numeric(ControlType::OperatingHours, 0., f32::MAX)
            .read_only(true)
            .unit("h"),
    );

    controls.insert(
        ControlType::RotationSpeed,
        Control::new_numeric(ControlType::RotationSpeed, 0., 990.)
            .read_only(true)
            .unit("dRPM"),
    );

    controls.insert(
        ControlType::FirmwareVersion,
        Control::new_string(ControlType::FirmwareVersion),
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
    controls.set_model_name("Furuno".to_string());

    let mut control = Control::new_string(ControlType::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlType::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Furuno ... ' name.
    if radar_info.user_name() == radar_info.key() {
        let mut user_name = "Furuno".to_string();
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

    controls.insert(
        ControlType::NoTransmitStart1,
        Control::new_numeric(ControlType::NoTransmitStart1, -180., 180.)
            .wire_scale_factor(1800., true)
            .wire_offset(-1.),
    );
    controls.insert(
        ControlType::NoTransmitEnd1,
        Control::new_numeric(ControlType::NoTransmitEnd1, -180., 180.)
            .unit("Deg")
            .wire_scale_factor(1800., true)
            .wire_offset(-1.),
    );

    controls.insert(
        ControlType::ScanSpeed,
        Control::new_list(ControlType::ScanSpeed, &["Normal", "Fast"]),
    );
    controls.insert(
        ControlType::TargetExpansion,
        Control::new_list(ControlType::TargetExpansion, &["Off", "On"]),
    );
    controls.insert(
        ControlType::TargetSeparation,
        Control::new_list(
            ControlType::TargetSeparation,
            &["Off", "Low", "Medium", "High"],
        ),
    );
    controls.insert(
        ControlType::Doppler,
        Control::new_list(ControlType::Doppler, &["Off", "Normal", "Approaching"]),
    );
    controls.insert(
        ControlType::DopplerSpeedThreshold,
        Control::new_numeric(ControlType::DopplerSpeedThreshold, 0., 1000.),
    );
}
