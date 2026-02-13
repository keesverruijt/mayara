//
// Translation into JavaScript of the Rust enums in src/lib/settings.js
//

export { toSI, unitLabel, toUser, isMetric, toRangeValue, formatRangeValue };

function formatRangeValue(metric, range) {
  let [unit, text] = toRangeValue(metric, range);
  return text + " " + unitLabel(unit);
}

function dividesNear(a, b) {
  let remainder = a % b;
  let r = remainder <= 1.0 || remainder >= b - 1;
  console.log("dividesNear: " + a + " % " + b + " = " + remainder + " -> " + r);
  return r;
}

function isMetric(v) {
  if (v <= 100) {
    return dividesNear(v, 25);
  } else if (v <= 750) {
    return dividesNear(v, 50);
  }
  return dividesNear(v, 500);
}

const NAUTICAL_MILE = 1852.0;

function toRangeValue(metric, v) {
  if (metric) {
    // Metric
    v = Math.round(v);
    if (v >= 1000) {
      return [Units.KiloMeters, v / 1000];
    } else {
      return [Units.Meters, v];
    }
  } else {
    if (v >= NAUTICAL_MILE - 1) {
      if (dividesNear(v, NAUTICAL_MILE)) {
        return [Units.NauticalMiles, Math.floor((v + 1) / NAUTICAL_MILE)];
      } else {
        return [Units.NauticalMiles, v / NAUTICAL_MILE];
      }
    } else if (dividesNear(v, NAUTICAL_MILE / 2)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 2)) + "/2",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 4)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 4)) + "/4",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 8)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 8)) + "/8",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 16)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 16)) + "/16",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 32)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 32)) + "/32",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 64)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 64)) + "/64",
      ];
    } else if (dividesNear(v, NAUTICAL_MILE / 128)) {
      return [
        Units.NauticalMiles,
        Math.floor((v + 1) / (NAUTICAL_MILE / 128)) + "/128",
      ];
    } else {
      return [Units.NauticalMiles, v / NAUTICAL_MILE];
    }
  }
}

const Units = Object.freeze({
  None: "None",
  Meters: "m",
  KiloMeters: "km",
  NauticalMiles: "nm",
  MetersPerSecond: "m/s",
  Knots: "kn",
  Degrees: "deg",
  Radians: "rad",
  RadiansPerSecond: "rad/s",
  RotationsPerMinute: "rpm",
  Seconds: "s",
  Minutes: "min",
  Hours: "h",
});

// Helper: get the short label for a unit
function unitLabel(unit) {
  return Units[unit] ?? "";
}

// -----------------------------------------------------------
//  3) Conversion table (from, to, factor)
// -----------------------------------------------------------
const TO_SI_CONVERSIONS = [
  [Units.NauticalMiles, Units.Meters, 1852.0],
  [Units.KiloMeters, Units.Meters, 1000.0],
  [Units.Knots, Units.MetersPerSecond, 1852.0 / 3600.0],
  [Units.Degrees, Units.Radians, Math.PI / 180.0],
  [Units.RotationsPerMinute, Units.RadiansPerSecond, (2.0 * Math.PI) / 60.0],
  [Units.Minutes, Units.Seconds, 60.0],
  [Units.Hours, Units.Seconds, 3600.0],
];

const TO_USER_CONVERSIONS = [
  [Units.Knots, Units.MetersPerSecond, 3600.0 / 1852.0],
  [Units.Degrees, Units.Radians, 180.0 / Math.PI],
  [Units.RotationsPerMinute, Units.RadiansPerSecond, 60.0 / (2.0 * Math.PI)],
  [Units.Hours, Units.Seconds, 1 / 3600.0],
];

function toSI(unit, value) {
  for (const [from, to, factor] of TO_SI_CONVERSIONS) {
    if (unit === from) return [to, value * factor];
  }
  // No conversion needed – already SI or unknown unit
  return [unit, value];
}

function toUser(unit, value) {
  // Special case to prefer nm only if it matches a particular list
  if (unit === Units.Meters) {
    let [probeUnit, probeValue] = toRangeValue(false, value);
    if (probeUnit === Units.NauticalMiles) {
      return [Units.NauticalMiles, value];
    }
  }
  for (const [to, from, factor] of TO_USER_CONVERSIONS) {
    if (unit === from) return [to, value * factor];
  }
  // No conversion needed
  return [unit, value];
}

function fromSI(targetUnit, originSI, value) {
  for (const [from, to, factor] of TO_SI_CONVERSIONS) {
    if (targetUnit === from && originSI === to) return value / factor;
  }
  // No conversion needed – already target unit or unknown
  return value;
}

// -----------------------------------------------------------
//  7) Reverse map: short label → Units enum value
// -----------------------------------------------------------
const LabelToUnit = Object.freeze(
  // Convert UnitLabels into an object where key=label, value=unit
  Object.entries(Units).reduce((acc, [unit, label]) => {
    if (label !== "") {
      // skip the empty string for None
      acc[label] = unit;
    }
    return acc;
  }, {})
);

// -----------------------------------------------------------
//  8) Public helper: parse a string into a Units value
// -----------------------------------------------------------
/**
 * @param {string} label   e.g. "km", "deg", "m/s"
 * @returns {string | null}  Returns the matching Units enum value or null if unknown
 */
function parseUnit(label) {
  return LabelToUnit[label] ?? null;
}
