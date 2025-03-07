use std::collections::HashMap;

use crate::{
    radar::RadarInfo,
    settings::{
        AutomaticValue, Control, ControlDestination, ControlType, SharedControls,
        HAS_AUTO_NOT_ADJUSTABLE,
    },
};

use super::Model;

pub fn new(model: Option<&str>, replay: bool) -> SharedControls {
    let mut controls = HashMap::new();

    controls.insert(
        ControlType::UserName,
        Control::new_string(ControlType::UserName).read_only(false),
    );
    let mut control = Control::new_string(ControlType::ModelName);
    if model.is_some() {
        control.set_string(model.unwrap().to_string());
    }
    controls.insert(ControlType::ModelName, control);

    controls.insert(
        ControlType::AntennaHeight,
        Control::new_numeric(ControlType::AntennaHeight, 0., 9900.)
            .wire_scale_factor(99000., false) // we report cm but network has mm
            .unit("cm"),
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
        ControlType::InterferenceRejection,
        Control::new_list(
            ControlType::InterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );
    controls.insert(
        ControlType::LocalInterferenceRejection,
        Control::new_list(
            ControlType::LocalInterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );
    controls.insert(
        ControlType::Rain,
        Control::new_numeric(ControlType::Rain, 0., 100.).wire_scale_factor(255., false),
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

    SharedControls::new(controls, replay)
}

pub fn update_when_model_known(controls: &SharedControls, model: Model, radar_info: &RadarInfo) {
    controls.set_model_name(model.to_string());

    let mut control = Control::new_string(ControlType::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlType::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Navico ... ' name.
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

    let max_value = (match model {
        Model::Unknown => 96.,
        Model::BR24 => 24.,
        Model::Gen3 => 36.,
        Model::Gen4 => 48.,
        Model::HALO => 96.,
    }) * 1852.;
    let mut range_control = Control::new_numeric(ControlType::Range, 50., max_value)
        .unit("m")
        .wire_scale_factor(10. * max_value, true); // Radar sends and receives in decimeters
    if let Some(range_detection) = &radar_info.range_detection {
        if range_detection.complete {
            range_control.set_valid_values(range_detection.ranges.clone());
        }
    };
    controls.insert(ControlType::Range, range_control);

    if model == Model::HALO {
        controls.insert(
            ControlType::Mode,
            Control::new_list(
                ControlType::Mode,
                &["Custom", "Harbor", "Offshore", "Buoy", "Weather", "Bird"],
            ),
        );
        controls.insert(
            ControlType::AccentLight,
            Control::new_list(ControlType::AccentLight, &["Off", "Low", "Medium", "High"]),
        );

        for (_, start, end) in super::BLANKING_SETS {
            controls.insert(
                start,
                Control::new_numeric(start, -180., 180.)
                    .unit("Deg")
                    .wire_scale_factor(1800., true)
                    .wire_offset(-1.)
                    .has_enabled(),
            );
            controls.insert(
                end,
                Control::new_numeric(end, -180., 180.)
                    .unit("Deg")
                    .wire_scale_factor(1800., true)
                    .wire_offset(-1.)
                    .has_enabled(),
            );
        }

        controls.insert(
            // TODO: Investigate mapping on 4G
            ControlType::SeaState,
            Control::new_list(ControlType::SeaState, &["Calm", "Moderate", "Rough"]),
        );

        controls.insert(
            ControlType::Sea,
            Control::new_auto(
                ControlType::Sea,
                0.,
                100.,
                AutomaticValue {
                    has_auto: true,
                    has_auto_adjustable: true,
                    auto_adjust_min_value: -50.,
                    auto_adjust_max_value: 50.,
                },
            ),
        );
    } else {
        controls.insert(
            ControlType::Sea,
            Control::new_auto(ControlType::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(255., false),
        );
    }

    controls.insert(
        ControlType::ScanSpeed,
        Control::new_list(
            ControlType::ScanSpeed,
            if model == Model::HALO {
                &["Normal", "Medium", "Medium Plus", "Fast"]
            } else {
                &["Normal", "Medium", "Medium-High"]
            },
        ),
    );
    controls.insert(
        ControlType::TargetExpansion,
        Control::new_list(
            ControlType::TargetExpansion,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "On"]
            },
        ),
    );
    controls.insert(
        ControlType::NoiseRejection,
        Control::new_list(
            ControlType::NoiseRejection,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );
    if model == Model::HALO || model == Model::Gen4 {
        controls.insert(
            ControlType::TargetSeparation,
            Control::new_list(
                ControlType::TargetSeparation,
                &["Off", "Low", "Medium", "High"],
            ),
        );
    }
    if model == Model::HALO {
        controls.insert(
            ControlType::Doppler,
            Control::new_list(ControlType::Doppler, &["Off", "Normal", "Approaching"]),
        );
        controls.insert(
            ControlType::DopplerSpeedThreshold,
            Control::new_numeric(ControlType::DopplerSpeedThreshold, 0., 99.)
                .wire_scale_factor(99. * 16., true)
                .unit("cm/s"),
        );
        controls.insert(
            ControlType::DopplerTrailsOnly,
            Control::new_list(ControlType::DopplerTrailsOnly, &["Off", "On"])
                .set_destination(ControlDestination::Data),
        );
    }

    controls.insert(
        ControlType::NoiseRejection,
        Control::new_list(
            ControlType::NoiseRejection,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );
}
