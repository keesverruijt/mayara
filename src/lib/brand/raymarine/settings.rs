use std::collections::HashMap;

use crate::{
    Cli,
    brand::raymarine::RaymarineModel,
    radar::{NAUTICAL_MILE_F64, RadarInfo},
    settings::{Control, ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls},
};

use super::BaseModel;

pub fn new(args: &Cli, model: BaseModel) -> SharedControls {
    let mut controls = HashMap::new();

    let mut control = Control::new_string(ControlId::UserName);
    control.set_string(model.to_string());
    controls.insert(ControlId::UserName, control.read_only(false));

    let mut control = Control::new_string(ControlId::ModelName);
    control.set_string(model.to_string());
    controls.insert(ControlId::ModelName, control);

    controls.insert(
        ControlId::BearingAlignment,
        Control::new_numeric(ControlId::BearingAlignment, -180., 180.)
            .unit("Deg")
            .wire_scale_factor(1800., true)
            .wire_offset(-1.),
    );
    controls.insert(
        ControlId::Gain,
        Control::new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(100., false),
    );
    controls.insert(
        ControlId::InterferenceRejection,
        Control::new_list(
            ControlId::InterferenceRejection,
            &["Off", "Level 1", "Level 2", "Level 3", "Level 4", "Level 5"],
        ),
    );

    controls.insert(
        ControlId::Rain,
        Control::new_numeric(ControlId::Rain, 0., 100.).has_enabled(),
    );

    controls.insert(
        ControlId::RotationSpeed,
        Control::new_numeric(ControlId::RotationSpeed, 0., 99.)
            .wire_scale_factor(990., true) // 0.1 RPM
            .read_only(true)
            .unit("RPM"),
    );

    match model {
        BaseModel::Quantum => {
            controls.insert(
                ControlId::Mode,
                Control::new_list(
                    ControlId::Mode,
                    &["Harbor", "Coastal", "Offshore", "Weather"],
                ),
            );
            controls.insert(
                ControlId::Doppler,
                Control::new_list(ControlId::Doppler, &["Off", "On"]),
            );
            controls.insert(
                ControlId::TargetExpansion,
                Control::new_list(ControlId::TargetExpansion, &["Off", "On"]),
            );
            controls.insert(
                ControlId::ColorGain,
                Control::new_auto(ControlId::ColorGain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                    .wire_scale_factor(100., false),
            );
            controls.insert(
                ControlId::MainBangSuppression,
                Control::new_list(ControlId::MainBangSuppression, &["Off", "On"]),
            );
            controls.insert(
                ControlId::NoTransmitStart1,
                Control::new_numeric(ControlId::NoTransmitStart1, 0., 359.)
                    .unit("Deg")
                    .wire_scale_factor(3590., true)
                    .has_enabled(),
            );
            controls.insert(
                ControlId::NoTransmitEnd1,
                Control::new_numeric(ControlId::NoTransmitEnd1, 0., 359.)
                    .unit("Deg")
                    .wire_scale_factor(3590., true)
                    .has_enabled(),
            );
            controls.insert(
                ControlId::NoTransmitStart2,
                Control::new_numeric(ControlId::NoTransmitStart2, 0., 359.)
                    .unit("Deg")
                    .wire_scale_factor(3590., true)
                    .has_enabled(),
            );
            controls.insert(
                ControlId::NoTransmitEnd2,
                Control::new_numeric(ControlId::NoTransmitEnd2, 0., 359.)
                    .unit("Deg")
                    .wire_scale_factor(3590., true)
                    .has_enabled(),
            );
            controls.insert(
                ControlId::SeaClutterCurve,
                Control::new_numeric(ControlId::SeaClutterCurve, 1., 2.),
            );
        }
        BaseModel::RD => {
            controls.insert(
                ControlId::MagnetronCurrent,
                Control::new_numeric(ControlId::MagnetronCurrent, 0., 65535.).read_only(true),
            );
            controls.insert(
                ControlId::DisplayTiming,
                Control::new_numeric(ControlId::DisplayTiming, 0., 255.).read_only(true),
            );
            controls.insert(
                ControlId::SignalStrength,
                Control::new_numeric(ControlId::SignalStrength, 0., 255.).read_only(true),
            );
            controls.insert(
                ControlId::WarmupTime,
                Control::new_numeric(ControlId::WarmupTime, 0., 255.)
                    .has_enabled()
                    .read_only(true),
            );
            controls.insert(
                ControlId::Tune,
                Control::new_auto(ControlId::Tune, 0., 255., HAS_AUTO_NOT_ADJUSTABLE)
                    .wire_scale_factor(255., false)
                    .read_only(true),
            );

            let mut control =
                Control::new_numeric(ControlId::Ftc, 0., 100.).wire_scale_factor(100., false);
            if model == BaseModel::RD {
                control = control.has_enabled();
            }
            controls.insert(ControlId::Ftc, control);
        }
    }
    SharedControls::new(args, controls)
}

pub fn update_when_model_known(
    controls: &mut SharedControls,
    model: &RaymarineModel,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.name.to_string());

    let mut control = Control::new_string(ControlId::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlId::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Raymarine ... ' name.
    if controls.user_name() == radar_info.key() {
        let mut user_name = model.name.to_string();
        if radar_info.serial_no.is_some() {
            let serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial);
        }
        if radar_info.which.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.which.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    let max_value = 36. * NAUTICAL_MILE_F64 as f32;
    let mut range_control = Control::new_numeric(ControlId::Range, 50., max_value).unit("m");
    range_control.set_valid_ranges(&radar_info.ranges);
    controls.insert(ControlId::Range, range_control);

    controls.insert(
        ControlId::Sea,
        Control::new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(255., false),
    );

    controls.insert(
        ControlId::TargetExpansion,
        Control::new_list(ControlId::TargetExpansion, &["Off", "On"]),
    );
}
