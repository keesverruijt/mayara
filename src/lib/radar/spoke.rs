use std::f64::consts::PI;

use crate::{protos::RadarMessage::radar_message::Spoke, radar::SpokeBearing};

pub(crate) type GenericSpoke = Vec<u8>;

pub(crate) fn to_protobuf_spoke(
    spokes_per_revolution: u16,
    range: u32,
    angle: SpokeBearing,
    heading: Option<u16>,
    time: Option<u64>,
    generic_spoke: GenericSpoke,
) -> Spoke {
    log::trace!(
        "Spoke {}/{:?}/{} len {}",
        range,
        heading,
        angle,
        generic_spoke.len()
    );

    let heading = if heading.is_some() {
        heading.map(|h| (((h / 2) + angle) % spokes_per_revolution) as u32)
    } else {
        let heading = crate::navdata::get_heading_true();
        heading.map(|h| {
            (((h * spokes_per_revolution as f64 / (2. * PI)) as u16 + angle)
                % spokes_per_revolution) as u32
        })
    };

    let mut spoke = Spoke::new();
    spoke.range = range;
    spoke.angle = angle as u32;
    spoke.bearing = heading;

    (spoke.lat, spoke.lon) = crate::navdata::get_position_i64();
    spoke.time = time;
    spoke.data = generic_spoke;

    spoke
}
