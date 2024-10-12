use super::{Legend, SpokeBearing, BLOB_HISTORY_COLORS};

const MARGIN: usize = 100;

pub struct TrailBuffer {
    legend: Legend,
    spokes: usize,
    max_spoke_len: usize,
    trail_size: usize,
    true_trails: Vec<u8>,
    relative_trails: Vec<u16>,
    trail_length_ms: u32,
    rotation_speed_ms: u32,
    minimal_legend_value: u8,
}

impl TrailBuffer {
    pub fn new(legend: Legend, spokes: usize, max_spoke_len: usize) -> Self {
        let minimal_legend_value = legend.strong_return;
        let trail_size = max_spoke_len * 2 + MARGIN * 2;
        TrailBuffer {
            legend,
            spokes,
            max_spoke_len,
            trail_size,
            true_trails: vec![0; trail_size * trail_size],
            relative_trails: vec![0; spokes * max_spoke_len],
            trail_length_ms: 0,
            rotation_speed_ms: 0,
            minimal_legend_value,
        }
    }

    pub fn set_relative_trails_revolutions(&mut self, seconds: u16) {
        self.trail_length_ms = seconds as u32 * 1000;
    }

    pub fn set_rotation_speed(&mut self, ms: u32) {
        self.rotation_speed_ms = ms;
    }

    pub fn update_relative_trails(&mut self, angle: SpokeBearing, data: &mut Vec<u8>) {
        if self.trail_length_ms == 0 {
            return;
        }
        let max_trail_value = (self.trail_length_ms / self.rotation_speed_ms) as u16;

        let trail = &mut self.relative_trails[angle as usize * self.max_spoke_len as usize
            ..(angle + 1) as usize * self.max_spoke_len];

        let mut radius = 0;

        if angle == 0 {
            log::debug!("Spoke before trails: {:?}", data);
        }

        let update_relative_motion = true; // motion == TARGET_MOTION_RELATIVE;

        while radius < data.len() {
            if data[radius] >= self.minimal_legend_value && data[radius] < self.legend.history_start
            {
                trail[radius] = 1;
            } else if trail[radius] > 0 {
                trail[radius] = trail[radius].wrapping_add(1); // Yes, we want overflow here after 65535 rotations
            }

            if update_relative_motion
                && data[radius] == 0
                && trail[radius] > 0
                && trail[radius] < max_trail_value
            {
                let mut index =
                    (trail[radius] * BLOB_HISTORY_COLORS as u16 / max_trail_value) as u8;
                if index >= BLOB_HISTORY_COLORS {
                    index = BLOB_HISTORY_COLORS;
                }
                if index < 1 {
                    index = 1;
                }

                data[radius] = self.legend.history_start + index - 1;
            }
            radius += 1;
        }
        while radius < self.max_spoke_len {
            trail[radius] = 0;
        }

        if angle == 0 {
            log::debug!("Trail after trails: {:?}", trail);
            log::debug!("Spoke after trails: {:?}", data);
        }
    }

    // zoom_factor > 1 -> zoom in, enlarge image
    pub fn zoom_relative_trails(&mut self, zoom_factor: f64) {
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

    pub fn clear(&mut self) {
        self.true_trails = vec![0; self.trail_size * self.trail_size];
        self.relative_trails = vec![0; self.spokes * self.max_spoke_len];
    }

    pub fn set_doppler_trail_only(&mut self, v: bool) {
        self.minimal_legend_value = if v {
            self.legend.doppler_approaching
        } else {
            self.legend.strong_return
        };
    }
}
