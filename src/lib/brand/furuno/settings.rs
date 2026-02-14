use std::collections::HashMap;

use crate::{
    Cli,
    radar::{NAUTICAL_MILE, RadarInfo, range::Ranges},
    settings::{
        ControlId, HAS_AUTO_NOT_ADJUSTABLE, SharedControls, Units, new_auto, new_list, new_numeric,
        new_string,
    },
};

use super::RadarModel;

pub fn new(args: &Cli) -> SharedControls {
    let mut controls = HashMap::new();

    new_string(ControlId::UserName)
        .read_only(false)
        .build(&mut controls);

    // Note: valid range values are set per-model in update_when_model_known()
    let max_value = 120. * NAUTICAL_MILE as f64;
    new_numeric(ControlId::Range, 0., max_value)
        .wire_units(Units::Meters)
        .build(&mut controls);

    new_auto(ControlId::Gain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_auto(ControlId::Sea, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_auto(ControlId::Rain, 0., 100., HAS_AUTO_NOT_ADJUSTABLE).build(&mut controls);
    new_numeric(ControlId::OperatingTime, 0., 999999999.)
        .read_only(true)
        .wire_units(Units::Seconds)
        .build(&mut controls);
    new_numeric(ControlId::TransmitTime, 0., 999999999.)
        .read_only(true)
        .wire_units(Units::Seconds)
        .build(&mut controls);

    new_numeric(ControlId::RotationSpeed, 0., 99.)
        .wire_scale_factor(10., true)
        .read_only(true)
        .wire_units(Units::RotationsPerMinute)
        .build(&mut controls);

    new_string(ControlId::SerialNumber).build(&mut controls);

    SharedControls::new(args, controls)
}

pub fn update_when_model_known(info: &mut RadarInfo, model: RadarModel, version: &str) {
    let model_name = model.to_string();
    log::debug!("update_when_model_known: {}", model_name);
    info.controls.set_model_name(model_name.to_string());

    // Update the UserName; it had to be present at start so it could be loaded from
    // config. Override it if it is still the original 'key' internal name.
    if info.controls.user_name() == info.key() {
        let mut user_name = model_name.to_string();
        if let Some(serial_no) = info.serial_no.as_deref() {
            user_name.push(' ');
            user_name.push_str(&serial_no[serial_no.len().saturating_sub(4)..]);
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
        .set_valid_ranges(&ControlId::Range, &ranges)
        .expect("Set valid values");

    // TODO: Add controls based on reverse engineered capability table

    info.controls.add(new_string(ControlId::FirmwareVersion));
    info.controls
        .set_string(&ControlId::FirmwareVersion, version.to_string())
        .expect("FirmwareVersion");

    info.controls.add(
        new_numeric(ControlId::NoTransmitStart1, -180., 180.)
            .wire_offset(-1.)
            .wire_units(Units::Degrees),
    );
    info.controls.add(
        new_numeric(ControlId::NoTransmitEnd1, -180., 180.)
            .wire_offset(-1.)
            .wire_units(Units::Degrees),
    );

    info.controls.add(
        new_numeric(ControlId::NoTransmitStart2, -180., 180.)
            .wire_offset(-1.)
            .wire_units(Units::Degrees),
    );
    info.controls.add(
        new_numeric(ControlId::NoTransmitEnd2, -180., 180.)
            .wire_offset(-1.)
            .wire_units(Units::Degrees),
    );

    // Add NXT-specific controls for NXT models
    if matches!(
        model,
        RadarModel::DRS4DNXT | RadarModel::DRS6ANXT | RadarModel::DRS12ANXT | RadarModel::DRS25ANXT
    ) {
        info.dual_range = true;

        // Noise Reduction (Signal Processing feature 3)
        info.controls
            .add(new_list(ControlId::NoiseRejection, &["Off", "On"]));

        // Interference Rejection (Signal Processing feature 0)
        info.controls
            .add(new_list(ControlId::InterferenceRejection, &["Off", "On"]));

        // Target Separation (RezBoost / Beam Sharpening)
        info.controls.add(new_list(
            ControlId::TargetSeparation,
            &["Off", "Low", "Medium", "High"],
        ));

        // Bird Mode
        info.controls.add(new_list(
            ControlId::BirdMode,
            &["Off", "Low", "Medium", "High"],
        ));

        // Doppler (Target Analyzer): Off, Target, Rain
        info.controls
            .add(new_list(ControlId::Doppler, &["Off", "Target", "Rain"]));
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
        model,
        ranges.len(),
        ranges
    );
    ranges
}
