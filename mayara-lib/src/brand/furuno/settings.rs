use std::collections::HashMap;

use crate::{
    radar::{range::Ranges, RadarInfo, NAUTICAL_MILE},
    settings::{Control, ControlType, DataUpdate, SharedControls},
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
    let mut range_control = Control::new_numeric(ControlType::Range, 0., max_value).unit("m");
    range_control.set_valid_values(FURUNO_RADAR_RANGES.into());
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
}

fn get_ranges_by_model(model: &RadarModel) -> Vec<i32> {
    let mut ranges = Vec::new();

    let allowed = get_ranges_nm_by_model(model);
    for i in 0..allowed.len() {
        if allowed[i] {
            ranges.push(FURUNO_RADAR_RANGES[i]);
        }
    }
    log::debug!("Model {} supports ranges {:?}", model.to_str(), ranges);
    return ranges;
}

// From MaxSea.Radar.BusinessObjects.RadarRanges
static FURUNO_RADAR_RANGES: [i32; 22] = [
    115,  // 1/16nm
    231,  // 1/8nm
    463,  // 1/4nm
    926,  // 1/2nm
    1389, // 3/4nm
    NAUTICAL_MILE,
    2778, // 1,5nm
    NAUTICAL_MILE * 2,
    NAUTICAL_MILE * 3,
    NAUTICAL_MILE * 4,
    NAUTICAL_MILE * 6,
    NAUTICAL_MILE * 8,
    NAUTICAL_MILE * 12,
    NAUTICAL_MILE * 16,
    NAUTICAL_MILE * 24,
    NAUTICAL_MILE * 32,
    NAUTICAL_MILE * 36,
    NAUTICAL_MILE * 48,
    NAUTICAL_MILE * 64,
    NAUTICAL_MILE * 72,
    NAUTICAL_MILE * 96,
    NAUTICAL_MILE * 120,
];

// See Far.Wrapper.SensorProperty._availableRangeCodeListsForNm etc.
fn get_ranges_nm_by_model(model: &RadarModel) -> &'static [bool; 22] {
    static RANGES_NM_UNKNOWN: [bool; 22] = [
        true, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, true, true, true, true, true, false,
    ];
    static RANGES_NM_FAR21X7: [bool; 22] = [
        false, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, false, true, false, false, true, false,
    ];
    static RANGES_NM_FAR3000: [bool; 22] = [
        false, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, false, true, false, false, true, false,
    ];
    static RANGES_NM_DRS4_DNXT: [bool; 22] = [
        true, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, true, false, false, false, false, false,
    ];
    static RANGES_NM_DRS6_ANXT: [bool; 22] = [
        true, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, true, true, true, true, false, false,
    ];
    static RANGES_NM_FAR15X3: [bool; 22] = [
        false, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, false, true, false, false, true, false,
    ];
    static RANGES_NM_FAR14X6: [bool; 22] = [
        false, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, false, true, false, false, true, false,
    ];
    static RANGES_NM_DRS12_ANXT: [bool; 22] = [
        true, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, true, true, true, true, false, false,
    ];
    static RANGES_NM_DRS25_ANXT: [bool; 22] = [
        true, true, true, true, true, true, true, true, true, true, true, true, true, true, true,
        true, true, true, true, true, false, false,
    ];

    match model {
        RadarModel::Unknown
        | RadarModel::DRS
        | RadarModel::FAR14x7
        | RadarModel::DRS4DL
        | RadarModel::DRS6AXCLASS => &RANGES_NM_UNKNOWN,
        RadarModel::FAR21x7 => &RANGES_NM_FAR21X7,
        RadarModel::FAR3000 => &RANGES_NM_FAR3000,
        RadarModel::DRS4DNXT => &RANGES_NM_DRS4_DNXT,
        RadarModel::DRS6ANXT => &RANGES_NM_DRS6_ANXT,
        RadarModel::FAR15x3 => &RANGES_NM_FAR15X3,
        RadarModel::FAR14x6 => &RANGES_NM_FAR14X6,
        RadarModel::DRS12ANXT => &RANGES_NM_DRS12_ANXT,
        RadarModel::DRS25ANXT => &RANGES_NM_DRS25_ANXT,
    }
}
