use std::collections::HashMap;

use crate::{
    radar::{RadarInfo, NAUTICAL_MILE_F64},
    settings::{Control, ControlType, SharedControls, HAS_AUTO_NOT_ADJUSTABLE},
    Session,
};

use super::Model;

pub fn new(session: Session, model: Model) -> SharedControls {
    let mut controls = HashMap::new();

    let mut control = Control::new_string(ControlType::UserName);
    control.set_string(model.to_string());
    controls.insert(ControlType::UserName, control.read_only(false));

    let mut control = Control::new_string(ControlType::ModelName);
    control.set_string(model.to_string());
    controls.insert(ControlType::ModelName, control);

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
            .wire_scale_factor(100., false),
    );
    controls.insert(
        ControlType::InterferenceRejection,
        Control::new_list(
            ControlType::InterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );

    controls.insert(
        ControlType::Rain,
        Control::new_numeric(ControlType::Rain, 0., 100.).wire_scale_factor(100., false),
    );

    controls.insert(
        ControlType::RotationSpeed,
        Control::new_numeric(ControlType::RotationSpeed, 0., 99.)
            .wire_scale_factor(990., true)
            .read_only(true)
            .unit("RPM"),
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
    control.set_valid_values([1, 2].to_vec()); // Only allow setting to Standby (index 1) and Transmit (index 2)

    controls.insert(ControlType::Status, control);

    match model {
        Model::Quantum => {
            controls.insert(
                ControlType::Mode,
                Control::new_list(
                    ControlType::Mode,
                    &["Harbor", "Coastal", "Offshore", "Weather"],
                ),
            );
            controls.insert(
                ControlType::TargetExpansion,
                Control::new_list(ControlType::TargetExpansion, &["Off", "On"]),
            );
            controls.insert(
                ControlType::MainBangSuppression,
                Control::new_list(ControlType::MainBangSuppression, &["Off", "On"]),
            );
        }
        Model::RD => {}
    }
    SharedControls::new(session, controls)
}

pub fn update_when_model_known(
    controls: &mut SharedControls,
    model: Model,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.to_string());

    let mut control = Control::new_string(ControlType::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlType::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Raymarine ... ' name.
    if controls.user_name() == radar_info.key() {
        let mut user_name = model.to_string();
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

    match model {
        Model::Quantum => {}
        _ => {}
    }

    let max_value = 36. * NAUTICAL_MILE_F64 as f32;
    let range_control = Control::new_numeric(ControlType::Range, 50., max_value).unit("m");
    controls.insert(ControlType::Range, range_control);

    controls.insert(
        ControlType::Sea,
        Control::new_auto(ControlType::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(255., false),
    );

    controls.insert(
        ControlType::TargetExpansion,
        Control::new_list(ControlType::TargetExpansion, &["Off", "On"]),
    );
}
