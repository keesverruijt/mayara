use std::collections::HashMap;

use strum::VariantNames;

use super::{HaloMode, Model};
use crate::{
    Cli,
    radar::{NAUTICAL_MILE_F64, RadarInfo},
    settings::{
        AutomaticValue, Control, ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, Units,
    },
};

pub fn new(args: &Cli, model: Option<&str>) -> SharedControls {
    let mut controls = HashMap::new();

    let mut control = Control::new_string(ControlId::ModelName);
    if model.is_some() {
        control.set_string(model.unwrap().to_string());
    }
    controls.insert(ControlId::ModelName, control);

    controls.insert(
        ControlId::AntennaHeight,
        Control::new_numeric(ControlId::AntennaHeight, 0., 99.)
            .wire_scale_factor(1000., false)
            .wire_scale_step(100.) // Allow control in decimeters
            .wire_unit(Units::Meters),
    );
    controls.insert(
        ControlId::BearingAlignment,
        Control::new_numeric(ControlId::BearingAlignment, -180., 180.)
            .wire_unit(Units::Degrees)
            .wire_scale_factor(10., true)
            .wire_offset(-1.),
    );
    controls.insert(
        ControlId::Gain,
        Control::new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
            .wire_scale_factor(2.55, false),
    );
    controls.insert(
        ControlId::InterferenceRejection,
        Control::new_list(
            ControlId::InterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );
    controls.insert(
        ControlId::LocalInterferenceRejection,
        Control::new_list(
            ControlId::LocalInterferenceRejection,
            &["Off", "Low", "Medium", "High"],
        ),
    );
    controls.insert(
        ControlId::Rain,
        Control::new_numeric(ControlId::Rain, 0., 100.).wire_scale_factor(2.55, false),
    );
    controls.insert(
        ControlId::TargetBoost,
        Control::new_list(ControlId::TargetBoost, &["Off", "Low", "High"]),
    );

    controls.insert(
        ControlId::OperatingHours,
        Control::new_numeric(ControlId::OperatingHours, 0., 999999.)
            .read_only(true)
            .wire_scale_factor(3600., false)
            .wire_unit(Units::Seconds),
    );

    controls.insert(
        ControlId::RotationSpeed,
        Control::new_numeric(ControlId::RotationSpeed, 0., 99.)
            .wire_scale_step(0.1)
            .read_only(true)
            .wire_unit(Units::RotationsPerMinute),
    );

    controls.insert(
        ControlId::FirmwareVersion,
        Control::new_string(ControlId::FirmwareVersion),
    );

    controls.insert(
        ControlId::SideLobeSuppression,
        Control::new_auto(
            ControlId::SideLobeSuppression,
            0.,
            100.,
            HAS_AUTO_NOT_ADJUSTABLE,
        )
        .wire_scale_factor(2.55, false),
    );

    SharedControls::new(args, controls)
}

pub fn update_when_model_known(controls: &SharedControls, model: Model, radar_info: &RadarInfo) {
    controls.set_model_name(model.to_string());

    let mut control = Control::new_string(ControlId::SerialNumber);
    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        control.set_string(serial_number.to_string());
    }
    controls.insert(ControlId::SerialNumber, control);

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the 'Navico ... ' name.
    if controls.user_name() == radar_info.key() {
        let mut user_name = model.to_string();
        if radar_info.serial_no.is_some() {
            let mut serial = radar_info.serial_no.clone().unwrap();

            user_name.push(' ');
            user_name.push_str(&serial.split_off(7));
        }
        if radar_info.dual.is_some() {
            user_name.push(' ');
            user_name.push_str(&radar_info.dual.as_ref().unwrap());
        }
        controls.set_user_name(user_name);
    }

    let max_value = (match model {
        Model::Unknown => 96.,
        Model::BR24 => 24.,
        Model::Gen3 => 36.,
        Model::Gen4 | Model::HaloOrG4 => 48.,
        Model::HALO => 96.,
    }) * NAUTICAL_MILE_F64;
    let mut range_control = Control::new_numeric(ControlId::Range, 50., max_value)
        .wire_unit(Units::Meters)
        .wire_scale_factor(10., false); // Radar sends and receives in decimeters
    range_control.set_valid_ranges(&radar_info.ranges);
    controls.insert(ControlId::Range, range_control);

    if model == Model::HALO {
        controls.insert(
            ControlId::Mode,
            Control::new_list(ControlId::Mode, HaloMode::VARIANTS),
        );
        controls.insert(
            ControlId::AccentLight,
            Control::new_list(ControlId::AccentLight, &["Off", "Low", "Medium", "High"]),
        );

        for (_, start, end) in super::BLANKING_SETS {
            controls.insert(
                start,
                Control::new_numeric(start, -180., 180.)
                    .wire_unit(Units::Degrees)
                    .wire_scale_factor(10., true)
                    .wire_offset(-1.)
                    .has_enabled(),
            );
            controls.insert(
                end,
                Control::new_numeric(end, -180., 180.)
                    .wire_unit(Units::Degrees)
                    .wire_scale_factor(10., true)
                    .wire_offset(-1.)
                    .has_enabled(),
            );
        }

        controls.insert(
            // TODO: Investigate mapping on 4G
            ControlId::SeaState,
            Control::new_list(ControlId::SeaState, &["Calm", "Moderate", "Rough"]),
        );

        controls.insert(
            ControlId::Sea,
            Control::new_auto(
                ControlId::Sea,
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
            ControlId::Sea,
            Control::new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(2.55, false),
        );
    }

    controls.insert(
        ControlId::ScanSpeed,
        Control::new_list(
            ControlId::ScanSpeed,
            if model == Model::HALO {
                &["Normal", "Medium", "Medium Plus", "Fast"]
            } else {
                &["Normal", "Medium", "Medium-High"]
            },
        ),
    );
    controls.insert(
        ControlId::TargetExpansion,
        Control::new_list(
            ControlId::TargetExpansion,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "On"]
            },
        ),
    );
    controls.insert(
        ControlId::NoiseRejection,
        Control::new_list(
            ControlId::NoiseRejection,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );
    if model == Model::HALO || model == Model::Gen4 {
        controls.insert(
            ControlId::TargetSeparation,
            Control::new_list(
                ControlId::TargetSeparation,
                &["Off", "Low", "Medium", "High"],
            ),
        );
    }
    if model == Model::HALO {
        controls.insert(
            ControlId::Doppler,
            Control::new_list(ControlId::Doppler, &["Off", "Normal", "Approaching"]),
        );
        controls.insert(
            ControlId::DopplerAutoTrack,
            Control::new_list(ControlId::DopplerAutoTrack, &["Off", "On"]),
        );
        controls.insert(
            ControlId::DopplerSpeedThreshold,
            Control::new_numeric(ControlId::DopplerSpeedThreshold, 0., 15.94)
                .wire_scale_step(0.01)
                .wire_unit(Units::MetersPerSecond),
        );
        controls.insert(
            ControlId::DopplerTrailsOnly,
            Control::new_list(ControlId::DopplerTrailsOnly, &["Off", "On"]),
        );
    }

    controls.insert(
        ControlId::NoiseRejection,
        Control::new_list(
            ControlId::NoiseRejection,
            if model == Model::HALO {
                &["Off", "Low", "Medium", "High"]
            } else {
                &["Off", "Low", "High"]
            },
        ),
    );

    log::debug!("update_when_model_known: {:?}", controls);
}
