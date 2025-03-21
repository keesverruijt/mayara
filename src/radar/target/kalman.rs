use nalgebra::{SMatrix, SVector};
use std::{f64::consts::PI, ops::Add};

use crate::radar::GeoPosition;

const NOISE: f64 = 1.0;
// original value 0.015, larger values make target better follow change of
// target heading But too large value makes target adapt to any change of
// heading immediately, causing instability 2.0 seems to follow fast (20+ knts)
// pilot boat close in-shore Allowed covariance of target speed in lat and lon.
// critical for the performance of target tracking
// lower value makes target go straight
// higher values allow target to make curves

const CONVERT: f64 = (((1. / 1852.) / 1852.) / 60.) / 60.; // converts meters ^ 2 to degrees ^ 2

type Matrix2x2 = SMatrix<f64, 2, 2>;
type Matrix4x4 = SMatrix<f64, 4, 4>;
type Matrix4x2 = SMatrix<f64, 4, 2>;
type Matrix2x4 = SMatrix<f64, 2, 4>;

#[derive(Debug, Clone, Copy)]
pub struct Polar {
    pub angle: i32,
    pub r: i32,
    pub time: u64, // time of measurement
}

impl Polar {
    pub fn new(angle: i32, r: i32, time: u64) -> Self {
        Polar { angle, r, time }
    }

    pub fn angle_in_rad(&self, spokes: f64) -> f64 {
        self.angle as f64 * PI / 180. / spokes
    }
}

impl Add for Polar {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Polar {
            angle: self.angle + other.angle,
            r: self.r + other.r,
            time: self.time + other.time,
        }
    }
}

pub struct LocalPosition {
    pub pos: GeoPosition,
    pub dlat_dt: f64,      // latitude  of speed vector, m/s
    pub dlon_dt: f64,      // longitude of speed vector, m/s
    pub sd_speed_m_s: f64, // standard deviation of the speed, m/s
}

impl LocalPosition {
    pub fn new(pos: GeoPosition, dlat_dt: f64, dlon_dt: f64) -> Self {
        Self {
            pos,
            dlat_dt,
            dlon_dt,
            sd_speed_m_s: 0.,
        }
    }
}

#[derive(Debug, Clone)]
pub struct KalmanFilter {
    a: Matrix4x4,
    at: Matrix4x4,
    w: Matrix4x2,
    wt: Matrix2x4,
    h: Matrix2x4,
    ht: Matrix4x2,
    p: Matrix4x4,
    q: Matrix2x2,
    r: Matrix2x2,
    k: Matrix4x2,
    i: Matrix4x4,
    pub spokes: f64,
}

impl KalmanFilter {
    // as the measurement to state transformation is non-linear, the extended Kalman filter is used
    // as the state transformation is linear, the state transformation matrix F is equal to the jacobian A
    // f is the state transformation function Xk <- Xk-1
    // Ai,j is jacobian matrix dfi / dxj

    pub fn new(spokes: usize) -> Self {
        let mut f = KalmanFilter {
            a: Matrix4x4::identity(),
            at: Matrix4x4::identity(),
            w: Matrix4x2::zeros(),
            wt: Matrix2x4::zeros(),
            h: Matrix2x4::zeros(),
            ht: Matrix4x2::zeros(),
            p: Matrix4x4::zeros(),
            q: Matrix2x2::zeros(),
            r: Matrix2x2::zeros(),
            k: Matrix4x2::zeros(),
            i: Matrix4x4::identity(),
            spokes: spokes as f64,
        };
        f.reset_filter();
        f
    }

    pub fn reset_filter(&mut self) {
        // reset the filter to use  it for a new case
        self.a = Matrix4x4::identity();
        self.at = Matrix4x4::identity();

        // Jacobian matrix of partial derivatives dfi / dwj
        self.w = Matrix4x2::zeros();
        self.w[(2, 0)] = 1.;
        self.w[(3, 1)] = 1.;

        // transpose of W
        self.wt = self.w.transpose();

        // Observation matrix, jacobian of observation function h
        // dhi / dvj
        // angle = atan2 (lat,lon) * self.spokes / (2 * pi) + v1
        // r = sqrt(x * x + y * y) + v2
        // v is measurement noise
        self.h = Matrix2x4::zeros();

        // Transpose of observation matrix
        self.ht = Matrix4x2::zeros();

        // Jacobian V, dhi / dvj
        // As V is the identity matrix, it is left out of the calculation of the Kalman gain

        // P estimate error covariance
        // initial values follow
        // P(1, 1) = .0000027 * range * range;   ???
        self.p = Matrix4x4::zeros();
        self.p[(0, 0)] = 20.;
        self.p[(1, 1)] = 20.;
        self.p[(2, 2)] = 4.;
        self.p[(3, 3)] = 4.;
        // P(1, 1) = .00027 * range * range;   ???

        // Q Process noise covariance matrix
        //((((1. / 1852.) / 1852.) / 60.) / 60.) // converts meters ^ 2 to degrees ^ 2
        self.q[(0, 0)] = NOISE;
        // variance in lat speed, (m / sec)2. This variable controls the rate of turn of targets and how fast targets pick up speed
        self.q[(1, 1)] = NOISE;
        // variance in lon speed, (m / sec)2. This variable controls the rate of turn of targets and how fast targets pick up speed

        // R measurement noise covariance matrix
        self.r[(0, 0)] = 100.0; // variance in the angle 3.0
        self.r[(1, 1)] = 25.; // variance in radius  .5
    }

    pub fn predict(&mut self, xx: &mut LocalPosition, delta_time: f64) {
        let mut x = SMatrix::<f64, 4, 1>::new(xx.pos.lat, xx.pos.lon, xx.dlat_dt, xx.dlon_dt);
        self.a[(0, 2)] = delta_time; // time in seconds
        self.a[(1, 3)] = delta_time;

        self.at[(2, 0)] = delta_time;
        self.at[(3, 1)] = delta_time;

        x = self.a * x;
        xx.pos.lat = x[(0, 0)];
        xx.pos.lon = x[(1, 0)];
        xx.dlat_dt = x[(2, 0)];
        xx.dlon_dt = x[(3, 0)];
        xx.sd_speed_m_s = ((self.p[(2, 2)] + self.p[(3, 3)]) / 2.).sqrt(); // rough approximation of standard dev of speed
    }

    pub fn update_p(&mut self) {
        // calculate apriori P
        // separated from the predict to prevent the update being done both in pass1 and pass2

        self.p = self.a * self.p * self.at + self.w * self.q * self.wt;
    }

    pub fn set_measurement(
        &mut self,
        pol: &mut Polar,
        local_position: &mut LocalPosition,
        expected: &Polar,
        scale: f64,
    ) {
        // pol measured angular position
        // x expected local position, this is the position returned and to be used
        // expected, same but in polar coordinates
        let q_sum: f64 = local_position.pos.lon * local_position.pos.lon
            + local_position.pos.lat * local_position.pos.lat;

        let c: f64 = self.spokes / (2. * PI);
        self.h[(0, 0)] = -c * local_position.pos.lon / q_sum;
        self.h[(0, 1)] = c * local_position.pos.lat / q_sum;

        let q_sum = q_sum.sqrt();
        self.h[(1, 0)] = local_position.pos.lat / q_sum * scale;
        self.h[(1, 1)] = local_position.pos.lon / q_sum * scale;

        self.ht = self.h.transpose();

        let mut a = (pol.angle - expected.angle) as f64; // Z is  difference between measured and expected
        if a > self.spokes / 2. {
            a -= self.spokes;
        }
        if a < -self.spokes / 2. {
            a += self.spokes;
        }
        let b = (pol.r - expected.r) as f64;
        let z = SMatrix::<f64, 2, 1>::new(a, b);

        let mut x = SVector::<f64, 4>::new(
            local_position.pos.lat,
            local_position.pos.lon,
            local_position.dlat_dt,
            local_position.dlon_dt,
        );

        // calculate Kalman gain
        self.k = self.p
            * self.ht
            * (((self.h * self.p * self.ht) + self.r)
                .try_inverse()
                .unwrap());

        // calculate apostriori expected position
        x = x + self.k * z;
        local_position.pos.lat = x[(0, 0)];
        local_position.pos.lon = x[(1, 0)];
        local_position.dlat_dt = x[(2, 0)];
        local_position.dlon_dt = x[(3, 0)];

        // update covariance P
        self.p = (self.i - self.k * self.h) * self.p;
        local_position.sd_speed_m_s = ((self.p[(2, 2)] + self.p[(3, 3)]) / 2.).sqrt();
        // rough approximation of standard dev of speed
    }
}
