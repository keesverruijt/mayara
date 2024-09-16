use std::collections::HashMap;

use crate::{
    radar::RadarInfo,
    settings::{AutomaticValue, Control, ControlType, Controls},
};

use super::Model;

pub type NavicoControls = Controls;

impl NavicoControls {
    pub fn new(model: Option<&str>) -> Self {
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
            Control::new_numeric(ControlType::AntennaHeight, 0, 9900)
                .wire_scale_factor(99000) // we report cm but network has mm
                .unit("cm"),
        );
        controls.insert(
            ControlType::BearingAlignment,
            Control::new_numeric(ControlType::BearingAlignment, -180, 180)
                .unit("Deg")
                .wire_scale_factor(1800)
                .wire_offset(-1),
        );
        controls.insert(
            ControlType::Gain,
            Control::new_auto(
                ControlType::Gain,
                0,
                100,
                AutomaticValue {
                    has_auto: true,
                    auto_values: 100,
                    auto_descriptions: None,
                    has_auto_adjustable: true,
                    auto_adjust_min_value: -50,
                    auto_adjust_max_value: 50,
                },
            )
            .wire_scale_factor(255),
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
            Control::new_numeric(ControlType::Rain, 0, 100).wire_scale_factor(255),
        );
        controls.insert(
            ControlType::TargetBoost,
            Control::new_list(ControlType::TargetBoost, &["Off", "Low", "High"]),
        );

        controls.insert(
            ControlType::OperatingHours,
            Control::new_numeric(ControlType::OperatingHours, 0, i32::MAX)
                .read_only(true)
                .unit("h"),
        );

        controls.insert(
            ControlType::RotationSpeed,
            Control::new_numeric(ControlType::RotationSpeed, 0, 990)
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
        );
        control.set_valid_values([1, 2].to_vec());
        controls.insert(ControlType::Status, control);

        controls.insert(
            ControlType::SideLobeSuppression,
            Control::new_auto(
                ControlType::SideLobeSuppression,
                0,
                100,
                AutomaticValue {
                    has_auto: true,
                    auto_values: 1,
                    auto_descriptions: None,
                    has_auto_adjustable: false,
                    auto_adjust_min_value: 0,
                    auto_adjust_max_value: 0,
                },
            )
            .wire_scale_factor(255),
        );

        Controls::new_base(controls)
    }

    pub fn update_when_model_known(&mut self, model: Model, radar_info: &RadarInfo) {
        let controls = self;

        controls.set_model_name(model.to_string());

        let mut control = Control::new_string(ControlType::SerialNumber);
        if let Some(serial_number) = radar_info.serial_no.as_ref() {
            control.set_string(serial_number.to_string());
        }
        controls.insert(ControlType::SerialNumber, control);

        // Update the UserName; it had to be present at start so it could be loaded from
        // config. Override it if it is still the 'Navico ... ' name.
        if radar_info.user_name() == radar_info.key() {
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
            Model::Unknown => 0,
            Model::BR24 => 24,
            Model::Gen3 => 36,
            Model::Gen4 => 48,
            Model::HALO => 96,
        }) * 1852;
        let mut range_control = Control::new_numeric(ControlType::Range, 0, max_value)
            .unit("m")
            .wire_scale_factor(10 * max_value); // Radar sends and receives in decimeters
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
                    &["Custom", "Harbor", "Offshore", "Unknown", "Weather", "Bird"],
                ),
            );
            controls.insert(
                ControlType::AccentLight,
                Control::new_list(ControlType::AccentLight, &["Off", "Low", "Medium", "High"]),
            );

            controls.insert(
                ControlType::NoTransmitStart1,
                Control::new_numeric(ControlType::NoTransmitStart1, -180, 180)
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitStart2,
                Control::new_numeric(ControlType::NoTransmitStart2, -180, 180)
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitStart3,
                Control::new_numeric(ControlType::NoTransmitStart3, -180, 180)
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitStart4,
                Control::new_numeric(ControlType::NoTransmitStart4, -180, 180)
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitEnd1,
                Control::new_numeric(ControlType::NoTransmitEnd1, -180, 180)
                    .unit("Deg")
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitEnd2,
                Control::new_numeric(ControlType::NoTransmitEnd2, -180, 180)
                    .unit("Deg")
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitEnd3,
                Control::new_numeric(ControlType::NoTransmitEnd3, -180, 180)
                    .unit("Deg")
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );
            controls.insert(
                ControlType::NoTransmitEnd4,
                Control::new_numeric(ControlType::NoTransmitEnd4, -180, 180)
                    .unit("Deg")
                    .wire_scale_factor(1800)
                    .wire_offset(-1),
            );

            controls.insert(
                // TODO: Investigate mapping on 4G
                ControlType::SeaState,
                Control::new_list(ControlType::SeaState, &["Calm", "Moderate", "Rough"]),
            );

            controls.insert(
                ControlType::Sea,
                Control::new_auto(
                    ControlType::Sea,
                    0,
                    100,
                    AutomaticValue {
                        has_auto: true,
                        auto_values: 100,
                        auto_descriptions: None,
                        has_auto_adjustable: true,
                        auto_adjust_min_value: -50,
                        auto_adjust_max_value: 50,
                    },
                )
                .wire_scale_factor(255),
            );
        } else {
            controls.insert(
                ControlType::Sea,
                Control::new_auto(
                    ControlType::Sea,
                    0,
                    100,
                    AutomaticValue {
                        has_auto: true,
                        auto_values: 1,
                        auto_descriptions: None,
                        has_auto_adjustable: false,
                        auto_adjust_min_value: 0,
                        auto_adjust_max_value: 0,
                    },
                )
                .wire_scale_factor(255),
            );
        }

        controls.insert(
            ControlType::ScanSpeed,
            Control::new_list(
                ControlType::ScanSpeed,
                if model == Model::HALO {
                    &["Normal", "Medium", "", "Fast"]
                } else {
                    &["Normal", "Fast"]
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
                Control::new_numeric(ControlType::DopplerSpeedThreshold, 0, 1594)
                    .wire_scale_factor(1594 * 16)
                    .unit("cm/s"),
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
}
