use std::collections::HashMap;

use crate::settings::{
    AutomaticValue, Control, ControlMessage, ControlType, ControlValue, Controls,
};

use super::Model;

pub type NavicoControls = Controls;

impl NavicoControls {
    pub fn new2(
        model: Model,
        protobuf_tx: tokio::sync::broadcast::Sender<Vec<u8>>,
        control_tx: tokio::sync::broadcast::Sender<ControlValue>,
        command_tx: tokio::sync::broadcast::Sender<ControlMessage>,
    ) -> Self {
        let mut controls = HashMap::new();

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
                ControlType::ModelName,
                Control::new_string(ControlType::ModelName),
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
        }

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
            Control::new_numeric(ControlType::Rain, 0, 100),
        );

        let max_value = (match model {
            Model::Unknown => 0,
            Model::BR24 => 24,
            Model::Gen3 => 36,
            Model::Gen4 => 48,
            Model::HALO => 96,
        }) * 1852;
        controls.insert(
            ControlType::Range,
            Control::new_numeric(ControlType::Range, 0, max_value)
                .unit("m")
                .wire_scale_factor(10 * max_value), // Radar sends and receives in decimeters
        );

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
            ),
        );
        controls.insert(
            ControlType::SideLobeSuppression,
            Control::new_auto(
                ControlType::SideLobeSuppression,
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
            ControlType::TargetBoost,
            Control::new_list(ControlType::TargetBoost, &["Off", "Low", "High"]),
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
            ControlType::OperatingHours,
            Control::new_numeric(ControlType::OperatingHours, 0, i32::MAX)
                .read_only()
                .unit("h"),
        );

        controls.insert(
            ControlType::SerialNumber,
            Control::new_string(ControlType::SerialNumber),
        );

        controls.insert(
            ControlType::FirmwareVersion,
            Control::new_string(ControlType::FirmwareVersion),
        );

        controls.insert(
            ControlType::Status,
            Control::new_list(
                ControlType::Status,
                &["Off", "Standby", "Transmit", "", "", "SpinningUp"],
            ),
        );

        Controls::new(controls, protobuf_tx, control_tx, command_tx)
    }
}
