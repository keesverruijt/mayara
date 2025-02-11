use ndarray::{Array2, ArrayBase, Dim, OwnedRepr};

#[derive(Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy)]
pub struct PointInt {
    pub x: i16,
    pub y: i16,
}

pub struct PolarToCartesianLookup {
    spokes: usize,
    spoke_len: usize,
    xy: ArrayBase<OwnedRepr<Point>, Dim<[usize; 2]>>,
    xyi: ArrayBase<OwnedRepr<PointInt>, Dim<[usize; 2]>>,
}

impl PolarToCartesianLookup {
    pub fn new(spokes: usize, spoke_len: usize) -> Self {
        let mut xy = Vec::with_capacity(spokes * spoke_len);
        let mut xyi = Vec::with_capacity(spokes * spoke_len);
        for arc in 0..spokes {
            let sine = (arc as f32 * 2.0 * std::f32::consts::PI / spokes as f32).sin();
            let cosine = (arc as f32 * 2.0 * std::f32::consts::PI / spokes as f32).cos();
            for radius in 0..spoke_len {
                let x = radius as f32 * cosine;
                let y = radius as f32 * sine;
                xy.push(Point { x, y });
                xyi.push(PointInt {
                    x: x as i16,
                    y: y as i16,
                });
            }
        }
        let xy = Array2::from_shape_vec((spokes, spoke_len), xy).unwrap();
        let xyi = Array2::from_shape_vec((spokes, spoke_len), xyi).unwrap();
        PolarToCartesianLookup {
            spokes,
            spoke_len,
            xy,
            xyi,
        }
    }

    // We trust that the optimizer will inline this
    pub fn get_point(&self, angle: usize, radius: usize) -> &Point {
        let angle = (angle + self.spokes) % self.spokes;
        &self.xy[[angle, radius]]
    }

    pub fn get_point_int(&self, angle: usize, radius: usize) -> &PointInt {
        let angle = (angle + self.spokes) % self.spokes;
        &self.xyi[[angle, radius]]
    }
}
