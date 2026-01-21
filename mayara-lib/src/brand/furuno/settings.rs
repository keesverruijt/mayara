use std::collections::HashMap;

use crate::{
    radar::{range::Ranges, RadarInfo, NAUTICAL_MILE},
    settings::{Control, ControlType, SharedControls},
    Session,
};

use super::{RadarModel, FURUNO_SPOKES};

pub fn new(session: Session) -> SharedControls {
    let mut controls = HashMap::new();

    controls.insert(
        ControlType::UserName,
        Control::new_string(ControlType::UserName).read_only(false),
    );

    let max_value = 120. * NAUTICAL_MILE as f32;
    let range_control = Control::new_numeric(ControlType::Range, 0., max_value).unit("m");
    // Note: valid range values are set per-model in update_when_model_known()
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

    if log::log_enabled!(log::Level::Debug) {
        controls.insert(
            ControlType::Spokes,
            Control::new_numeric(ControlType::Spokes, 0., FURUNO_SPOKES as f32)
                .read_only(true)
                .unit("per rotation"),
        );
    }

    SharedControls::new(session, controls)
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

    let ranges = Ranges::new_by_distance(&get_ranges_by_model(&model));
    log::info!(
        "{}: model {} supports ranges {}",
        info.key(),
        model_name,
        ranges
    );
    info.controls
        .set_valid_ranges(&ControlType::Range, &ranges)
        .expect("Set valid values");

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

    // Add NXT-specific controls for NXT models
    if matches!(
        model,
        RadarModel::DRS4DNXT | RadarModel::DRS6ANXT | RadarModel::DRS12ANXT | RadarModel::DRS25ANXT
    ) {
        // Noise Reduction (Signal Processing feature 3)
        info.controls.insert(
            ControlType::NoiseRejection,
            Control::new_numeric(ControlType::NoiseRejection, 0., 1.)
                .unit("boolean"),
        );

        // Interference Rejection (Signal Processing feature 0)
        info.controls.insert(
            ControlType::InterferenceRejection,
            Control::new_numeric(ControlType::InterferenceRejection, 0., 1.)
                .unit("boolean"),
        );

        // Target Separation (RezBoost / Beam Sharpening)
        info.controls.insert(
            ControlType::TargetSeparation,
            Control::new_numeric(ControlType::TargetSeparation, 0., 3.)
                .unit("level"),
        );

        // Bird Mode
        info.controls.insert(
            ControlType::BirdMode,
            Control::new_numeric(ControlType::BirdMode, 0., 3.)
                .unit("level"),
        );

        // Doppler (Target Analyzer): Off, Target, Rain
        info.controls.insert(
            ControlType::Doppler,
            Control::new_list(ControlType::Doppler, &["Off", "Target", "Rain"]),
        );
    }
}

/// Range table for DRS-NXT series (in meters)
/// Ranges: 1/16, 1/8, 1/4, 1/2, 3/4, 1, 1.5, 2, 3, 4, 6, 8, 12, 16, 24, 32, 36, 48 NM
static RANGE_TABLE_DRS_NXT: &[i32] = &[
    116,   // 1/16 NM
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    59264, // 32 NM
    66672, // 36 NM
    88896, // 48 NM
];

/// Extended range table for DRS12A/DRS25A-NXT (adds 64, 72, 96 NM)
static RANGE_TABLE_DRS_NXT_EXTENDED: &[i32] = &[
    116,    // 1/16 NM
    231,    // 1/8 NM
    463,    // 1/4 NM
    926,    // 1/2 NM
    1389,   // 3/4 NM
    1852,   // 1 NM
    2778,   // 1.5 NM
    3704,   // 2 NM
    5556,   // 3 NM
    7408,   // 4 NM
    11112,  // 6 NM
    14816,  // 8 NM
    22224,  // 12 NM
    29632,  // 16 NM
    44448,  // 24 NM
    59264,  // 32 NM
    66672,  // 36 NM
    88896,  // 48 NM
    118528, // 64 NM
    133344, // 72 NM
    177792, // 96 NM
];

/// Range table for standard DRS series (non-NXT, up to 36 NM)
static RANGE_TABLE_DRS: &[i32] = &[
    116,   // 1/16 NM
    231,   // 1/8 NM
    463,   // 1/4 NM
    926,   // 1/2 NM
    1389,  // 3/4 NM
    1852,  // 1 NM
    2778,  // 1.5 NM
    3704,  // 2 NM
    5556,  // 3 NM
    7408,  // 4 NM
    11112, // 6 NM
    14816, // 8 NM
    22224, // 12 NM
    29632, // 16 NM
    44448, // 24 NM
    59264, // 32 NM
    66672, // 36 NM
];

/// Range table for FAR series commercial radars (different range increments)
static RANGE_TABLE_FAR: &[i32] = &[
    231,    // 1/8 NM
    463,    // 1/4 NM
    926,    // 1/2 NM
    1389,   // 3/4 NM
    1852,   // 1 NM
    2778,   // 1.5 NM
    3704,   // 2 NM
    5556,   // 3 NM
    7408,   // 4 NM
    11112,  // 6 NM
    14816,  // 8 NM
    22224,  // 12 NM
    29632,  // 16 NM
    44448,  // 24 NM
    59264,  // 32 NM
    88896,  // 48 NM
    177792, // 96 NM
];

/// Get the range table for a specific model
fn get_ranges_by_model(model: &RadarModel) -> Vec<i32> {
    let range_table: &[i32] = match model {
        // DRS-NXT series with extended ranges
        RadarModel::DRS12ANXT | RadarModel::DRS25ANXT => RANGE_TABLE_DRS_NXT_EXTENDED,

        // DRS-NXT series (standard)
        RadarModel::DRS4DNXT | RadarModel::DRS6ANXT => RANGE_TABLE_DRS_NXT,

        // FAR series (commercial radars)
        RadarModel::FAR21x7
        | RadarModel::FAR3000
        | RadarModel::FAR15x3
        | RadarModel::FAR14x6
        | RadarModel::FAR14x7 => RANGE_TABLE_FAR,

        // Standard DRS series and unknown models
        RadarModel::Unknown | RadarModel::DRS | RadarModel::DRS4DL | RadarModel::DRS6AXCLASS => {
            RANGE_TABLE_DRS
        }
    };

    let ranges: Vec<i32> = range_table.to_vec();
    log::debug!(
        "Model {} supports {} ranges: {:?}",
        model.to_str(),
        ranges.len(),
        ranges
    );
    ranges
}
