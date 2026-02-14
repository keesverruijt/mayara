use std::collections::HashMap;

use strum::VariantNames;

use super::{HaloMode, Model};
use crate::{
    Cli,
    radar::{NAUTICAL_MILE_F64, RadarInfo},
    settings::{
        AutomaticValue, ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, Units, new_auto,
        new_list, new_numeric, new_string,
    },
};

pub fn new(args: &Cli, model: Option<&str>) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::ModelName).build(&mut controls);
    if model.is_some() {
        controls
            .get_mut(&ControlId::ModelName)
            .unwrap()
            .set_string(model.unwrap().to_string());
    }

    new_numeric(ControlId::AntennaHeight, 0., 99.)
        .wire_scale_factor(1000., false)
        .wire_scale_step(100.) // Allow control in decimeters
        .wire_units(Units::Meters)
        .build(&mut controls);
    new_numeric(ControlId::BearingAlignment, -180., 180.)
        .wire_scale_factor(10., true)
        .wire_offset(-1.)
        .wire_units(Units::Degrees)
        .build(&mut controls);
    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);
    new_list(
        ControlId::InterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    new_list(
        ControlId::LocalInterferenceRejection,
        &["Off", "Low", "Medium", "High"],
    )
    .build(&mut controls);
    new_numeric(ControlId::Rain, 0., 100.)
        .wire_scale_factor(2.55, false)
        .build(&mut controls);
    new_list(ControlId::TargetBoost, &["Off", "Low", "High"]).build(&mut controls);
    new_numeric(ControlId::TransmitTime, 0., 999999.)
        .read_only(true)
        .wire_units(Units::Hours)
        .build(&mut controls);
    new_numeric(ControlId::RotationSpeed, 0., 99.)
        .wire_scale_step(0.1)
        .read_only(true)
        .wire_units(Units::RotationsPerMinute)
        .build(&mut controls);
    new_string(ControlId::FirmwareVersion).build(&mut controls);
    new_auto(
        ControlId::SideLobeSuppression,
        0.,
        100.,
        HAS_AUTO_NOT_ADJUSTABLE,
    )
    .wire_scale_factor(2.55, false)
    .build(&mut controls);
    new_string(ControlId::SerialNumber).build(&mut controls);

    SharedControls::new(args, controls)
}

pub fn update_when_model_known(
    controls: &mut SharedControls,
    model: Model,
    radar_info: &RadarInfo,
) {
    controls.set_model_name(model.to_string());

    if let Some(serial_number) = radar_info.serial_no.as_ref() {
        controls
            .set_string(&&ControlId::SerialNumber, serial_number.to_string())
            .unwrap();
    }
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
    controls.add(
        new_numeric(ControlId::Range, 50., max_value)
            .wire_scale_factor(10., false)
            .wire_units(Units::Meters),
    ); // Radar sends and receives in decimeters
    controls
        .set_valid_ranges(&ControlId::Range, &radar_info.ranges)
        .expect("Valid ranges");

    if model == Model::HALO {
        controls.add(new_list(ControlId::Mode, HaloMode::VARIANTS));
        controls.add(new_list(
            ControlId::AccentLight,
            &["Off", "Low", "Medium", "High"],
        ));

        for (_, start, end) in super::BLANKING_SETS {
            controls.add(
                new_numeric(start, -180., 180.)
                    .wire_scale_factor(10., true)
                    .wire_offset(-1.)
                    .wire_units(Units::Degrees)
                    .has_enabled(),
            );
            controls.add(
                new_numeric(end, -180., 180.)
                    .wire_scale_factor(10., true)
                    .wire_offset(-1.)
                    .wire_units(Units::Degrees)
                    .has_enabled(),
            );
        }

        controls.add(new_list(
            ControlId::SeaState,
            &["Calm", "Moderate", "Rough"],
        ));

        controls.add(new_auto(
            ControlId::Sea,
            0.,
            100.,
            AutomaticValue {
                has_auto: true,
                has_auto_adjustable: true,
                auto_adjust_min_value: -50.,
                auto_adjust_max_value: 50.,
            },
        ));
    } else {
        controls.add(
            new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE)
                .wire_scale_factor(2.55, false),
        );
    }

    controls.add(new_list(
        ControlId::ScanSpeed,
        if model == Model::HALO {
            &["Normal", "Medium", "Medium Plus", "Fast"]
        } else {
            &["Normal", "Medium", "Medium-High"]
        },
    ));
    controls.add(new_list(
        ControlId::TargetExpansion,
        if model == Model::HALO {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "On"]
        },
    ));
    controls.add(new_list(
        ControlId::NoiseRejection,
        if model == Model::HALO {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "Low", "High"]
        },
    ));
    if model == Model::HALO || model == Model::Gen4 {
        controls.add(new_list(
            ControlId::TargetSeparation,
            &["Off", "Low", "Medium", "High"],
        ));
    }
    if model == Model::HALO {
        controls.add(new_list(
            ControlId::Doppler,
            &["Off", "Normal", "Approaching"],
        ));
        controls.add(new_list(ControlId::DopplerAutoTrack, &["Off", "On"]));
        controls.add(
            new_numeric(ControlId::DopplerSpeedThreshold, 0., 15.94)
                .wire_scale_step(0.01)
                .wire_units(Units::MetersPerSecond),
        );
        controls.add(new_list(ControlId::DopplerTrailsOnly, &["Off", "On"]));
    }

    controls.add(new_list(
        ControlId::NoiseRejection,
        if model == Model::HALO {
            &["Off", "Low", "Medium", "High"]
        } else {
            &["Off", "Low", "High"]
        },
    ));

    log::debug!("update_when_model_known: {:?}", controls);
}
