//
// Every radar supports a number of ranges, which are
// the distances at which the radar can detect objects.
//
// Some radars use the index into a well-defined list of ranges,
// while others use a numeric value that represents the range in meters.
// Navico, Raymarine and Garmin radars use a numeric value,
// while Furuno uses an index into a list of ranges.

use std::fmt::{Display, Formatter, Result as FmtResult};
use std::sync::LazyLock;

pub const NAUTICAL_MILE: i32 = 1852; // 1 nautical mile in meters

// All ranges seen on all radars
pub static ALL_POSSIBLE_NAUTICAL_RANGES: LazyLock<Ranges> = LazyLock::new(|| {
    Ranges::new(vec![
        Range::initial(57),                  // 1/32 nm
        Range::initial(115),                 // 1/16 nm
        Range::initial(231),                 // 1/8 nm
        Range::initial(346),                 // 3/16 nm
        Range::initial(462),                 // 1/4 nm
        Range::initial(693),                 // 3/8 nm
        Range::initial(926),                 // 1/2 nm
        Range::initial(1156),                // 5/8 nm
        Range::initial(1388),                // 3/4 nm
        Range::initial(NAUTICAL_MILE),       // 1 nm
        Range::initial(NAUTICAL_MILE + 462), // 1.25 nm
        Range::initial(NAUTICAL_MILE + 926), // 1.5 nm
        Range::initial(NAUTICAL_MILE * 2),   // 2 nm
        Range::initial(NAUTICAL_MILE * 3),   // 3 nm
        Range::initial(NAUTICAL_MILE * 4),   // 4 nm
        Range::initial(NAUTICAL_MILE * 6),   // 6 nm
        Range::initial(NAUTICAL_MILE * 8),   // 8 nm
        Range::initial(NAUTICAL_MILE * 12),  // 12 nm
        Range::initial(NAUTICAL_MILE * 16),  // 16 nm
        Range::initial(NAUTICAL_MILE * 24),  // 24 nm
        Range::initial(NAUTICAL_MILE * 32),  // 32 nm
        Range::initial(NAUTICAL_MILE * 36),  // 36 nm
        Range::initial(NAUTICAL_MILE * 40),  // 40 nm
        Range::initial(NAUTICAL_MILE * 48),  // 48 nm
        Range::initial(NAUTICAL_MILE * 64),  // 64 nm
        Range::initial(NAUTICAL_MILE * 72),  // 72 nm
        Range::initial(NAUTICAL_MILE * 96),  // 96 nm
        Range::initial(NAUTICAL_MILE * 120), // 120 nm
    ])
});

// All ranges seen on all radars
pub static ALL_POSSIBLE_METRIC_RANGES: LazyLock<Ranges> = LazyLock::new(|| {
    Ranges::new(vec![
        Range::initial(50),
        Range::initial(75),
        Range::initial(100),
        Range::initial(250),
        Range::initial(500),
        Range::initial(750),
        Range::initial(1000),
        Range::initial(1500),
        Range::initial(2000),
        Range::initial(3000),
        Range::initial(4000),
        Range::initial(6000),
        Range::initial(8000),
        Range::initial(12000),
        Range::initial(16000),
        Range::initial(24000),
        Range::initial(36000),
        Range::initial(48000),
        Range::initial(64000),
        Range::initial(72000),
        Range::initial(96000),
        Range::initial(120000),
    ])
});

#[derive(Debug, Clone, Copy, Eq, Ord)]
pub struct Range {
    distance: i32,
    index: usize,
}

impl Range {
    fn initial(value: i32) -> Self {
        Self {
            distance: value,
            index: 0,
        }
    }

    pub fn new(value: i32, index: usize) -> Self {
        Self {
            distance: value,
            index,
        }
    }

    pub fn value(&self) -> i32 {
        self.distance
    }

    pub fn index(&self) -> usize {
        self.index
    }

    fn near(a: i32, b: i32) -> bool {
        return a >= b - 1 && a <= b + 1 || (b == 0 && a == 99);
    }

    fn metric(v: i32) -> bool {
        Self::near(v % 100, 0) || Self::near(v, 25) || Self::near(v, 50) || Self::near(v, 75)
    }

    pub fn is_metric(&self) -> bool {
        Self::metric(self.distance)
    }

    pub fn is_nautical(&self) -> bool {
        !Self::metric(self.distance)
    }
}

impl PartialOrd for Range {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Range {
    fn eq(&self, other: &Self) -> bool {
        self.distance == other.distance
    }
}

impl Display for Range {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        {
            let v = self.distance;
            if Range::metric(v) {
                // Metric
                if v > 1000 {
                    if v % 1000 == 0 {
                        write!(f, "{} km", v / 1000)
                    } else {
                        write!(f, "{} km", v as f64 / 1000.0)
                    }
                } else {
                    write!(f, "{} m", v)
                }
            } else {
                if v % 1852 == 0 {
                    write!(f, "{} nm", v / 1852)
                } else if v >= 1852 && v % 1852 == 1852 / 2 {
                    write!(f, "{},5 nm", v / 1852)
                } else {
                    let s = match v {
                        56 | 57 | 58 => "1/32 nm",
                        114 | 115 | 116 => "1/16 nm",
                        230 | 231 | 232 => "1/8 nm",
                        346 | 347 | 348 => "3/16 nm",
                        462 | 463 | 464 => "1/4 nm",
                        693 | 694 | 695 => "3/8 nm",
                        925 | 926 | 927 => "1/2 nm",
                        1156 | 1157 | 1158 => "5/8 nm",
                        1388 | 1389 | 1390 => "3/4 nm",
                        2314 | 2315 | 2316 => "1,25 nm",
                        _ => "",
                    };
                    if s.len() > 0 {
                        write!(f, "{}", s)
                    } else {
                        // If we don't have a special case, just write the value in meters
                        write!(f, "{} m", v)
                    }
                }
            }
        }
    }
}

impl From<Range> for i32 {
    fn from(range: Range) -> Self {
        range.distance
    }
}
impl From<&Range> for i32 {
    fn from(range: &Range) -> Self {
        range.distance
    }
}
#[derive(Debug, Clone)]
pub struct Ranges {
    pub ranges: Vec<Range>,
    pub metric_ranges: Vec<Range>,
    pub nautical_ranges: Vec<Range>,
}

impl Ranges {
    pub fn new(mut ranges: Vec<Range>) -> Self {
        let mut metric_ranges = Vec::new();
        let mut nautical_ranges = Vec::new();
        let mut complete_ranges = Vec::new();
        ranges.sort_by(|a, b| a.distance.cmp(&b.distance));
        for (i, range) in ranges.iter().enumerate() {
            if Range::metric(range.distance) {
                metric_ranges.push(Range::new(range.distance, i));
            } else {
                nautical_ranges.push(Range::new(range.distance, i));
            }
            complete_ranges.push(Range::new(range.distance, i));
        }
        Self {
            ranges,
            metric_ranges,
            nautical_ranges,
        }
    }

    pub fn new_empty() -> Self {
        Self {
            ranges: Vec::new(),
            metric_ranges: Vec::new(),
            nautical_ranges: Vec::new(),
        }
    }

    pub fn new_by_distance(ranges: &Vec<i32>) -> Self {
        let mut r = Vec::new();
        for (i, &value) in ranges.iter().enumerate() {
            r.push(Range::new(value, i));
        }
        Self::new(r)
    }

    fn push(&mut self, range: Range) -> bool {
        if self.ranges.iter().any(|r| r.distance == range.distance) {
            // If the range already exists, do not add it again
            return false;
        }
        let index = self.ranges.len();
        self.ranges.push(range);
        if Range::metric(self.ranges[index].distance) {
            self.metric_ranges
                .push(Range::new(self.ranges[index].distance, index));
        } else {
            self.nautical_ranges
                .push(Range::new(self.ranges[index].distance, index));
        }
        true
    }

    pub fn get_distance(&self, index: usize) -> Option<&Range> {
        self.ranges.get(index)
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }
}

impl Display for Ranges {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        let mut first = true;
        for range in &self.ranges {
            if !first {
                write!(f, ", ")?;
            }
            write!(f, "{}", range.distance)?;
            first = false;
        }
        Ok(())
    }
}

pub enum RangeDetectionResult {
    NoRange,
    Complete(Ranges, i32),
    NextRange(i32),
}

#[derive(Clone, Debug)]
pub(crate) struct RangeDetection {
    key: String,
    saved_range: i32,
    min_range: i32,
    max_range: i32,
    ranges: Ranges,
    ranges_to_try: Ranges,
    index_to_try: usize,
}

impl RangeDetection {
    pub fn new(key: String, min_range: i32, max_range: i32, metric: bool, nautical: bool) -> Self {
        let mut ranges_to_try = Vec::new();
        if metric {
            ranges_to_try.extend(ALL_POSSIBLE_METRIC_RANGES.ranges.iter().cloned());
        }
        if nautical {
            ranges_to_try.extend(ALL_POSSIBLE_NAUTICAL_RANGES.ranges.iter().cloned());
        }

        log::info!("{key}: Trying all ranges between {min_range} and {max_range}");
        log::debug!("{key}: Ranges to try: {ranges_to_try:?}");
        RangeDetection {
            key,
            saved_range: 0,
            min_range,
            max_range,
            ranges: Ranges::new_empty(),
            ranges_to_try: Ranges::new(ranges_to_try),
            index_to_try: 0,
        }
    }

    ///
    /// Try the next range in the list of ranges to try.
    /// Returns false if there are no more ranges to try,
    ///
    fn advance_to_next_index(&mut self) -> Option<&Range> {
        while self.index_to_try < self.ranges_to_try.ranges.len() {
            let range = &self.ranges_to_try.ranges[self.index_to_try];
            log::debug!(
                "{}: advance_to_next_index i={} of {}",
                self.key,
                self.index_to_try,
                self.ranges_to_try.ranges.len(),
            );
            self.index_to_try += 1;
            if range.value() >= self.min_range && range.value() <= self.max_range {
                log::debug!(
                    "{}: advance_to_next_index found range {} m",
                    self.key,
                    range.value()
                );
                return Some(range);
            }
        }
        None
    }

    pub fn found_range(&mut self, range: i32) -> RangeDetectionResult {
        if range < self.min_range || range > self.max_range {
            RangeDetectionResult::NoRange
        } else {
            if self.saved_range == 0 {
                self.saved_range = range;
            }

            log::trace!("{}: reported range {} m", self.key, range);
            if self.ranges.push(Range::initial(range)) {
                log::info!("{}: Found range {}", self.key, range);
            }

            if let Some(range) = self.advance_to_next_index() {
                return RangeDetectionResult::NextRange(range.value());
            } else {
                self.ranges = Ranges::new(self.ranges.ranges.clone()); // Sort by distance
                log::info!("{}: Found supported ranges {}", self.key, self.ranges);
                return RangeDetectionResult::Complete(self.ranges.clone(), self.saved_range);
            }
        }
    }
}
