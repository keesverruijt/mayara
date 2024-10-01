use super::{Legend, SpokeBearing, BLOB_HISTORY_COLORS};

const MARGIN: usize = 100;

struct TrailBuffer {
    legend: Legend,
    spokes: usize,
    max_spoke_len: usize,
    previous_pixels_per_meter: f64,
    trail_size: usize,
    true_trails: Vec<u8>,
    relative_trails: Vec<u8>,
}

impl TrailBuffer {
    pub fn new(legend: Legend, spokes: u32, max_spoke_len: u32) -> Self {
        let trail_size: usize = max_spoke_len as usize * 2 + MARGIN * 2;
        TrailBuffer {
            legend,
            spokes: spokes as usize,
            max_spoke_len: spokes as usize,
            previous_pixels_per_meter: 0.,
            trail_size,
            true_trails: vec![0; trail_size * trail_size],
            relative_trails: vec![0; spokes as usize * max_spoke_len as usize],
        }
    }

    fn update_relative_trails(&mut self, angle: SpokeBearing, data: &mut Vec<u8>, len: usize) {
        let trail = &mut self.relative_trails[angle as usize * self.max_spoke_len as usize
            ..(angle + 1) as usize * self.max_spoke_len];

        let mut radius = 0;

        let update_relative_motion = true; // motion == TARGET_MOTION_RELATIVE;

        while radius < len {
            if data[radius] >= self.legend.strong_return {
                trail[radius] = 1;
            } else if trail[radius] > 0 && trail[radius] < BLOB_HISTORY_COLORS {
                trail[radius] += 1;
            }

            if update_relative_motion && data[radius] == 0 {
                data[radius] = self.legend.history_start + trail[radius];
            }
            radius += 1;
        }
        while radius < self.max_spoke_len {
            trail[radius] = 0;
        }
    }

    // zoom_factor > 1 -> zoom in, enlarge image
    fn zoom_relative_trails(&mut self, zoom_factor: f64) {
        let mut new_trail = vec![0; self.max_spoke_len];
        let mut index_prev = 0;
        for spoke in 0..self.spokes {
            {
                let trail = &self.relative_trails
                    [spoke * self.max_spoke_len..(spoke + 1) * self.max_spoke_len];

                for j in 0..self.max_spoke_len {
                    let index_new = (j as f64 * zoom_factor) as usize;
                    if index_new >= self.max_spoke_len {
                        break;
                    }
                    if trail[j] != 0 {
                        for k in index_prev..=index_new {
                            new_trail[k] = trail[j];
                        }
                    }
                    index_prev = index_new + 1;
                }
            }
            self.relative_trails[spoke * self.max_spoke_len..(spoke + 1) * self.max_spoke_len]
                .copy_from_slice(&new_trail);

            new_trail.fill(0);
        }
    }
}
