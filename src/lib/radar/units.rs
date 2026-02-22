use serde_string_enum::{DeserializeLabeledStringEnum, SerializeLabeledStringEnum};
use std::f64::consts::PI;
use utoipa::ToSchema;

#[derive(
    Copy,
    PartialEq,
    SerializeLabeledStringEnum,
    DeserializeLabeledStringEnum,
    Clone,
    Debug,
    ToSchema,
)]
pub enum Units {
    #[string = ""]
    None,
    #[string = "m"]
    Meters,
    #[string = "km"]
    KiloMeters,
    #[string = "nm"]
    NauticalMiles,
    #[string = "m/s"]
    MetersPerSecond,
    #[string = "kn"]
    Knots,
    #[string = "deg"]
    Degrees,
    #[string = "rad"]
    Radians,
    #[string = "rad/s"]
    RadiansPerSecond,
    #[string = "rpm"]
    RotationsPerMinute,
    #[string = "s"]
    Seconds,
    #[string = "min"]
    Minutes,
    #[string = "h"]
    Hours,
}

impl Units {
    pub(crate) fn to_si(&self, value: f64) -> (Units, f64) {
        let (units, factor) = match self {
            Units::Degrees => (Units::Radians, PI / 180.),
            Units::Hours => (Units::Seconds, 3600.),
            Units::Minutes => (Units::Seconds, 60.),
            Units::KiloMeters => (Units::Meters, 1000.),
            Units::Knots => (Units::MetersPerSecond, 1852. / 3600.),
            Units::Meters => (Units::Meters, 1.),
            Units::MetersPerSecond => (Units::MetersPerSecond, 1.),
            Units::NauticalMiles => (Units::Meters, 1852.),
            Units::None => unreachable!("Units::None"),
            Units::Radians => (Units::Radians, 1.),
            Units::RadiansPerSecond => (Units::RadiansPerSecond, 1.),
            Units::RotationsPerMinute => (Units::RotationsPerMinute, (2. * PI) / 60.),
            Units::Seconds => (Units::Seconds, 1.),
        };
        (units, value * factor)
    }

    pub(crate) fn from_si(&self, value: f64) -> f64 {
        let factor = match self {
            Units::Degrees => 180. / PI,
            Units::Hours => 1. / 3600.,
            Units::Minutes => 1. / 60.,
            Units::KiloMeters => 0.001,
            Units::Knots => 3600. / 1852.,
            Units::Meters => 1.,
            Units::MetersPerSecond => 1.,
            Units::NauticalMiles => 1. / 1852.,
            Units::None => unreachable!("Units::None"),
            Units::Radians => 1.,
            Units::RadiansPerSecond => 1.,
            Units::RotationsPerMinute => 60. / (2. * PI),
            Units::Seconds => 1.,
        };

        value * factor
    }
}
