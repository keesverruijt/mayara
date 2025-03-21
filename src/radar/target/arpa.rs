use std::cmp::max;
use std::cmp::min;
use std::f64::consts::PI;

use super::kalman::KalmanFilter;
use super::kalman::Polar;
use super::Doppler;
use super::ExtendedPosition;
use super::RefreshState;
use super::TargetBuffer;
use super::TargetStatus;
use super::FOUR_DIRECTIONS;
use super::MAX_CONTOUR_LENGTH;
use crate::radar::target::kalman::LocalPosition;
use crate::radar::target::meters_per_degree_longitude;
use crate::radar::target::MAX_LOST_COUNT;
use crate::radar::target::METERS_PER_DEGREE_LATITUDE;
use crate::radar::GeoPosition;

#[derive(Debug, Clone)]
struct Contour {
    length: i32,
    min_angle: i32,
    max_angle: i32,
    min_r: i32,
    max_r: i32,
    position: Polar,
    contour: Vec<Polar>,
}

impl Contour {
    fn new() -> Contour {
        Contour {
            length: 0,
            min_angle: 0,
            max_angle: 0,
            min_r: 0,
            max_r: 0,
            position: Polar::new(0, 0, 0),
            contour: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ArpaTarget {
    pub m_status: TargetStatus,

    m_average_contour_length: i32,
    m_small_fast: bool,
    m_previous_contour_length: i32,
    m_lost_count: i32,
    m_refresh_time: u64,
    m_automatic: bool,
    m_polar_pos: Polar,
    m_radar_pos: GeoPosition,
    m_course: f64,
    m_stationary: i32,
    m_doppler_target: Doppler,
    pub m_refreshed: RefreshState,
    m_target_id: u32,
    m_transferred_target: bool,
    m_kalman: KalmanFilter,
    contour: Contour,
    m_total_pix: u32,
    m_approaching_pix: u32,
    m_receding_pix: u32,
    have_doppler: bool,
    pub position: ExtendedPosition,
    expected: Polar,
    pub age_rotations: u32,
}

impl ArpaTarget {
    pub fn new(
        position: ExtendedPosition,
        uid: u32,
        spokes: usize,
        m_status: TargetStatus,
        have_doppler: bool,
    ) -> Self {
        // makes new target with an existing id
        Self {
            m_status,
            m_average_contour_length: 0,
            m_small_fast: false,
            m_previous_contour_length: 0,
            m_lost_count: 0,
            m_refresh_time: 0,
            m_automatic: false,
            m_polar_pos: Polar::new(0, 0, 0),
            m_radar_pos: GeoPosition::new(0., 0.),
            m_course: 0.,
            m_stationary: 0,
            m_doppler_target: Doppler::ANY,
            m_refreshed: RefreshState::NotFound,
            m_target_id: uid,
            m_transferred_target: false,
            m_kalman: KalmanFilter::new(spokes),
            contour: Contour::new(),
            m_total_pix: 0,
            m_approaching_pix: 0,
            m_receding_pix: 0,
            have_doppler,
            position,
            expected: Polar::new(0, 0, 0),
            age_rotations: 0,
        }
    }

    /**
     * Find a contour from the given start position on the edge of a blob.
     *
     * Follows the contour in a clockwise manner.
     *
     * Returns 0 if ok, or a small integer on error (but nothing is done with this)
     */
    fn get_contour(&mut self, buffer: &TargetBuffer, pol: &mut Polar) -> i32 {
        let mut count = 0;
        let start = *pol;
        let mut current = start;
        let mut next = current;

        let mut succes = false;
        let mut index = 0;

        let contour = &mut self.contour;
        contour.max_r = current.r;
        contour.max_angle = current.angle;
        contour.min_r = current.r;
        contour.min_angle = current.angle;
        contour.contour.clear();
        // check if p inside blob
        if start.r >= buffer.setup.spoke_len {
            return 1; // return code 1, r too large
        }
        if start.r < 4 {
            return 2; // return code 2, r too small
        }
        if !buffer.pix(&self.m_doppler_target, start.angle, start.r) {
            return 3; // return code 3, starting point outside blob
        }

        // first find the orientation of border point p
        for i in 0..4 {
            index = i;
            if !buffer.pix(
                &self.m_doppler_target,
                current.angle + FOUR_DIRECTIONS[index].angle,
                current.r + FOUR_DIRECTIONS[index].r,
            ) {
                succes = true;
                break;
            }
        }
        if !succes {
            return 4; // return code 4, starting point not on contour
        }
        index = (index + 1) % 4; // determines starting direction

        succes = false;
        while count < MAX_CONTOUR_LENGTH {
            // try all translations to find the next point
            // start with the "left most" translation relative to the previous one
            index = (index + 3) % 4; // we will turn left all the time if possible
            for _i in 0..4 {
                next = current + FOUR_DIRECTIONS[index];
                if buffer.pix(&self.m_doppler_target, next.angle, next.r) {
                    succes = true; // next point found

                    break;
                }
                index = (index + 1) % 4;
            }
            if !succes {
                return 7; // return code 7, no next point found
            }
            // next point found
            current = next;
            if count < MAX_CONTOUR_LENGTH - 1 {
                contour.contour.push(current);
            } else if count == MAX_CONTOUR_LENGTH - 1 {
                contour.contour.push(current);
                contour.contour.push(start); // shortcut to the beginning for drawing the contour
            }
            if current.angle > contour.max_angle {
                contour.max_angle = current.angle;
            }
            if current.angle < contour.min_angle {
                contour.min_angle = current.angle;
            }
            if current.r > contour.max_r {
                contour.max_r = current.r;
            }
            if current.r < contour.min_r {
                contour.min_r = current.r;
            }
            count += 1;
        }
        contour.length = contour.contour.len() as i32;

        //  CalculateCentroid(*target);    we better use the real centroid instead of the average, TODO
        contour.min_angle = buffer.mod_spokes(contour.min_angle);
        contour.max_angle = buffer.mod_spokes(contour.max_angle);

        if contour.max_r >= buffer.setup.spoke_len || contour.min_r >= buffer.setup.spoke_len {
            return 10; // return code 10 r too large
        }
        if contour.max_r < 2 || contour.min_r < 2 {
            return 11; // return code 11 r too small
        }
        pol.angle = buffer.mod_spokes((contour.max_angle + contour.min_angle) / 2);
        pol.r = (contour.max_r + contour.min_r) / 2;
        pol.time = buffer.history[pol.angle as usize].time;

        self.m_radar_pos = buffer.history[pol.angle as usize].pos;
        self.m_polar_pos = *pol;

        return 0; //  success, blob found
    }

    pub fn refresh_target(
        mut target: ArpaTarget,
        buffer: &mut TargetBuffer,
        dist: i32,
        pass: i32,
    ) -> Option<Self> {
        let setup = buffer.setup.clone();

        let prev_refresh = target.m_refresh_time;
        // refresh may be called from guard directly, better check
        let own_pos = crate::signalk::get_radar_position();
        if target.m_status == TargetStatus::LOST || own_pos.is_none() {
            target.m_refreshed = RefreshState::OutOfScope;
        }
        if target.m_refreshed == RefreshState::Found
            || target.m_refreshed == RefreshState::OutOfScope
        {
            return None;
        }

        let own_pos = ExtendedPosition::new(own_pos.unwrap(), 0., 0., 0, 0., 0.);

        let mut pol = buffer.pos2polar(&target.position, &own_pos);
        let alfa0 = pol.angle;
        let r0 = pol.r;
        let scan_margin = buffer.scan_margin();
        let angle_time = buffer.history[buffer.mod_spokes(pol.angle + scan_margin) as usize].time;
        // angle_time is the time of a spoke SCAN_MARGIN spokes forward of the target, if that spoke is refreshed we assume that the target has been refreshed

        // let now = wxGetUTCTimeMillis();  // millis
        let mut rotation_period = 2500; // TODO!
        if rotation_period == 0 {
            rotation_period = 2500; // default value
        }
        if angle_time < target.m_refresh_time + rotation_period - 100 {
            // the 100 is a margin on the rotation period
            // the next image of the target is not yet there
            target.m_refreshed = RefreshState::OutOfScope;
            return Some(target);
        }

        // set new refresh time
        target.m_refresh_time = buffer.history[pol.angle as usize].time;
        let prev_position = target.position.clone(); // save the previous target position

        // PREDICTION CYCLE

        log::debug!("Begin prediction cycle m_target_id={}, status={:?}, angle={}, r={}, contour={}, pass={}, lat={}, lon={}",
               target.m_target_id, target.m_status, pol.angle, pol.r, target.contour.length, pass, target.position.pos.lat, target.position.pos.lon);

        // estimated new target time
        let delta_t = if target.m_refresh_time >= prev_position.time
            && target.m_status != TargetStatus::Acquire0
        {
            (target.m_refresh_time - prev_position.time) as f64 / 1000. // in seconds
        } else {
            0.
        };

        if target.position.pos.lat > 90. || target.position.pos.lat < -90. {
            target.set_status_lost();
            target.m_refreshed = RefreshState::OutOfScope;
            log::trace!("Target {} has unlikely latitude", target.m_target_id);
            return Some(target);
        }

        let mut x_local = LocalPosition::new(
            GeoPosition::new(
                (target.position.pos.lat - own_pos.pos.lat) * 60. * 1852.,
                (target.position.pos.lon - own_pos.pos.lon)
                    * 60.
                    * 1852.
                    * own_pos.pos.lat.to_radians().cos(),
            ),
            target.position.dlat_dt,
            target.position.dlon_dt,
        );

        target.m_kalman.predict(&mut x_local, delta_t); // x_local is new estimated local position of the target
                                                        // now set the polar to expected angular position from the expected local position

        pol.angle = buffer.mod_spokes(
            (f64::atan2(x_local.pos.lon, x_local.pos.lat) * setup.spokes_f64 / (2. * PI)) as i32,
        );
        pol.r = ((x_local.pos.lat * x_local.pos.lat + x_local.pos.lon * x_local.pos.lon).sqrt()
            * setup.pixels_per_meter) as i32;

        // zooming and target movement may  cause r to be out of bounds
        log::trace!("PREDICTION m_target_id={}, pass={}, status={:?}, angle={}.{}, r={}.{}, contour={}, speed={}, sd_speed_kn={} doppler={:?}, lostcount={}",
               target.m_target_id, pass, target.m_status, alfa0, pol.angle, r0, pol.r, target.contour.length, target.position.speed_kn,
               target.position.sd_speed_kn, target.m_doppler_target, target.m_lost_count);
        if pol.r >= setup.spoke_len || pol.r <= 0 {
            target.m_refreshed = RefreshState::OutOfScope;
            // delete target if too far out
            log::trace!("R out of bounds, target deleted m_target_id={}, angle={}, r={}, contour={}, pass={}", target.m_target_id,
                 pol.angle, pol.r, target.contour.length, pass);
            target.set_status_lost();
            return Some(target);
        }
        target.expected = pol; // save expected polar position

        // MEASUREMENT CYCLE
        // now search for the target at the expected polar position in pol
        let mut dist1 = dist;

        const LAST_PASS: i32 = 2;

        if pass == LAST_PASS {
            // this is doubtfull $$$
            if target.m_status == TargetStatus::Acquire0
                || target.m_status == TargetStatus::Acquire1
            {
                dist1 *= 2;
            } else if target.position.speed_kn > 15. {
                dist1 *= 2;
            } /*else if (self.position.speed_kn > 30.) {
                dist1 *= 4;
              } */
        }

        let back = pol;

        // here we really search for the target
        if pass == LAST_PASS {
            target.m_doppler_target = Doppler::ANY; // in the last pass we are not critical
        }
        let mut found = target.get_target(buffer, &mut pol, dist1); // main target search

        let dist_angle = ((pol.angle - back.angle) as f64 * pol.r as f64 / 326.) as i32;
        let dist_radial = pol.r - back.r;
        let dist_total =
            ((dist_angle * dist_angle + dist_radial * dist_radial) as f64).sqrt() as i32;
        if found {
            log::debug!("id={}, Found dist_angle={}, dist_radial={}, dist_total={}, pol.angle={}, back.angle={}, doppler={:?}", 
        target.m_target_id,
                 dist_angle, dist_radial, dist_total, pol.angle, back.angle, target.m_doppler_target);

            if target.m_doppler_target != Doppler::ANY {
                let backup = target.m_doppler_target;
                target.m_doppler_target = Doppler::ANY;
                let _ = target.get_target(buffer, &mut pol, dist1); // get the contour for the target ins ANY state
                target.pixel_counter(buffer);
                target.m_doppler_target = backup;
                let _ = target.get_target(buffer, &mut pol, dist1); // restore target in original state
                target.state_transition(); // adapt state if required
            } else {
                target.pixel_counter(buffer);
                target.state_transition();
            }
            if target.m_average_contour_length != 0
                && (target.contour.length < target.m_average_contour_length / 2
                    || target.contour.length > target.m_average_contour_length * 2)
            {
                if pass <= LAST_PASS - 1 {
                    // Don't accept this hit
                    // Search again in next pass
                    log::debug!(
                        "id={}, reject weightedcontourlength={}, contour.length={}",
                        target.m_target_id,
                        target.m_average_contour_length,
                        target.contour.length
                    );
                    found = false;
                } else {
                    log::debug!(
                        "id={}, accept weightedcontourlength={}, contour.length={}",
                        target.m_target_id,
                        target.m_average_contour_length,
                        target.contour.length
                    );
                }
            }
        }
        if found {
            target.reset_pixels(buffer);
            log::debug!("target Found ResetPixels m_target_id={}, angle={}, r={}, contour={}, pass={}, doppler={:?}",
                 target.m_target_id, pol.angle, pol.r, target.contour.length, pass, target.m_doppler_target);
            if target.contour.length >= MAX_CONTOUR_LENGTH as i32 - 2 {
                // don't use this blob, could be radar interference
                // The pixels of the blob have been reset, so you won't find it again
                found = false;
                log::debug!("reset found because of max contour length id={}, angle={}, r={}, contour={}, pass={}", 
                target.m_target_id, pol.angle,
                 pol.r, target.contour.length, pass);
            }
        }

        if found {
            target.m_lost_count = 0;
            let mut p_own = ExtendedPosition::empty();
            p_own.pos = buffer.history[buffer.mod_spokes(pol.angle) as usize].pos;
            target.age_rotations += 1;
            target.m_status = match target.m_status {
                TargetStatus::Acquire0 => TargetStatus::Acquire1,
                TargetStatus::Acquire1 => TargetStatus::Acquire2,
                TargetStatus::Acquire2 => TargetStatus::ACQUIRE3,
                TargetStatus::ACQUIRE3 | TargetStatus::ACTIVE => TargetStatus::ACTIVE,
                _ => TargetStatus::Acquire0,
            };
            if target.m_status == TargetStatus::Acquire0 {
                // as this is the first measurement, move target to measured position
                // ExtendedPosition p_own;
                // p_own.pos = m_ri.m_history[MOD_SPOKES(pol.angle)].pos;  // get the position at receive time
                target.position = buffer.polar2pos(&pol, &mut p_own); // using own ship location from the time of reception, only lat and lon
                target.position.dlat_dt = 0.;
                target.position.dlon_dt = 0.;
                target.position.sd_speed_kn = 0.;
                target.expected = pol;
                log::debug!(
                    "calculated id={} pos={}",
                    target.m_target_id,
                    target.position.pos
                );
                target.age_rotations = 0;
            }

            // Kalman filter to  calculate the apostriori local position and speed based on found position (pol)
            if target.m_status == TargetStatus::Acquire2
                || target.m_status == TargetStatus::ACQUIRE3
            {
                target.m_kalman.update_p();
                target.m_kalman.set_measurement(
                    &mut pol,
                    &mut x_local,
                    &target.expected,
                    setup.pixels_per_meter,
                ); // pol is measured position in polar coordinates
            }
            // x_local expected position in local coordinates

            target.position.time = pol.time; // set the target time to the newly found time, this is the time the spoke was received

            if target.m_status != TargetStatus::Acquire1 {
                // if status == 1, then this was first measurement, keep position at measured position
                target.position.pos.lat =
                    own_pos.pos.lat + x_local.pos.lat / METERS_PER_DEGREE_LATITUDE;
                target.position.pos.lon = own_pos.pos.lon
                    + x_local.pos.lon / meters_per_degree_longitude(&own_pos.pos.lat);
                target.position.dlat_dt = x_local.dlat_dt; // meters / sec
                target.position.dlon_dt = x_local.dlon_dt; // meters /sec
                target.position.sd_speed_kn = x_local.sd_speed_m_s * 3600. / 1852.;
            }

            // Here we bypass the Kalman filter to predict the speed of the target
            // Kalman filter is too slow to adjust to the speed of (fast) new targets
            // This method however only works for targets where the accuricy of the position is high,
            // that is small targets in relation to the size of the target.

            if target.m_status == TargetStatus::Acquire2 {
                // determine if this is a small and fast target
                let dist_angle = pol.angle - alfa0;
                let dist_r = pol.r - r0;
                let size_angle = max(
                    buffer.mod_spokes(target.contour.max_angle - target.contour.min_angle),
                    1,
                );
                let size_r = max(target.contour.max_r - target.contour.min_r, 1);
                let test = (dist_r as f64 / size_r as f64).abs()
                    + (dist_angle as f64 / size_angle as f64).abs();
                target.m_small_fast = test > 2.;
                log::debug!("smallandfast, id={}, test={}, dist_r={}, size_r={}, dist_angle={}, size_angle={}",
          target.m_target_id, test, dist_r, size_r, dist_angle, size_angle);
            }

            const FORCED_POSITION_STATUS: u32 = 8;
            const FORCED_POSITION_AGE_FAST: u32 = 5;

            if target.m_small_fast
                && target.age_rotations >= 2
                && target.age_rotations < FORCED_POSITION_STATUS
                && (target.age_rotations < FORCED_POSITION_AGE_FAST
                    || target.position.speed_kn > 10.)
            {
                // Do a linear extrapolation of the estimated position instead of the kalman filter, as it
                // takes too long to get up to speed for these targets.
                let prev_pos = prev_position.pos;
                let new_pos = buffer.polar2pos(&pol, &p_own).pos;
                let delta_lat = new_pos.lat - prev_pos.lat;
                let delta_lon = new_pos.lon - prev_pos.lon;
                let delta_t = pol.time - prev_position.time;
                if delta_t > 1000 {
                    // delta_t < 1000; speed unreliable due to uncertainties in location
                    let d_lat_dt =
                        (delta_lat / (delta_t as f64)) * METERS_PER_DEGREE_LATITUDE * 1000.;
                    let d_lon_dt = (delta_lon / (delta_t as f64))
                        * meters_per_degree_longitude(&new_pos.lat)
                        * 1000.;
                    log::debug!("id={}, FORCED m_status={:?}, d_lat_dt={}, d_lon_dt={}, delta_lon_meter={}, delta_lat_meter={}, deltat={}",
                   target.m_target_id, target.m_status, d_lat_dt, d_lon_dt,
                     delta_lon * METERS_PER_DEGREE_LATITUDE, delta_lat * METERS_PER_DEGREE_LATITUDE, delta_t);
                    // force new position and speed, dependent of overridefactor

                    let factor: f64 = (0.8_f64).powf((target.age_rotations - 1) as f64);
                    target.position.pos.lat += factor * (new_pos.lat - target.position.pos.lat);
                    target.position.pos.lon += factor * (new_pos.lon - target.position.pos.lon);
                    target.position.dlat_dt += factor * (d_lat_dt - target.position.dlat_dt); // in meters/sec
                    target.position.dlon_dt += factor * (d_lon_dt - target.position.dlon_dt);
                    // in meters/sec
                }
            }

            // set refresh time to the time of the spoke where the target was found
            target.m_refresh_time = target.position.time;
            if target.age_rotations >= 1 {
                let s1 = target.position.dlat_dt; // m per second
                let s2 = target.position.dlon_dt; // m  per second
                target.position.speed_kn = (s1 * s1 + s2 * s2).sqrt() * 3600. / 1852.; // and convert to nautical miles per hour
                target.m_course = f64::atan2(s2, s1).to_degrees();
                if target.m_course < 0. {
                    target.m_course += 360.;
                }

                log::debug!("FOUND {} CYCLE id={}, status={:?}, age={}, angle={}.{}, r={}.{}, contour={}, speed={}, sd_speed_kn={}, doppler={:?}",
              pass, target.m_target_id, target.m_status, target.age_rotations, alfa0, pol.angle, r0, pol.r, target.contour.length, target.position.speed_kn,
              target.position.sd_speed_kn, target.m_doppler_target);

                target.m_previous_contour_length = target.contour.length;
                // send target data to OCPN and other radar
                if target.m_target_id == 0 {
                    target.m_target_id = buffer.get_next_target_id();
                }

                if target.age_rotations > FORCED_POSITION_STATUS {
                    let _ = buffer.pos2polar(&target.position, &own_pos);
                }
                if target.age_rotations > 4 { // 4? quite arbitrary, target should be stable
                     // TODO TransferTargetToOtherRadar();
                     // TODO SendTargetToNearbyRadar();
                }

                const WEIGHT_FACTOR: f64 = 0.1;

                if target.contour.length != 0 {
                    if target.m_average_contour_length == 0 && target.contour.length != 0 {
                        target.m_average_contour_length = target.contour.length;
                    } else {
                        target.m_average_contour_length +=
                            ((target.contour.length - target.m_average_contour_length) as f64
                                * WEIGHT_FACTOR) as i32;
                    }
                }

                //if (m_status >= m_ri.m_target_age_to_mixer.GetValue()) {
                //  f64 dist2target = pol.r / self.pixels_per_meter;
                // TODO: PassAIVDMtoOCPN(&pol);  // status s not yet used

                // TODO: MakeAndTransmitTargetMessage();

                // MakeAndTransmitCoT();
                //}
            }
            target.m_refreshed = RefreshState::Found;
            // A target that has been found is no longer considered a transferred target
            target.m_transferred_target = false;
        } else {
            // target not found
            log::debug!(
                "Not found id={}, angle={}, r={}, pass={}, lost_count={}, status={:?}",
                target.m_target_id,
                pol.angle,
                pol.r,
                pass,
                target.m_lost_count,
                target.m_status
            );
            // not found in pass 0 or 1 (An other chance will follow)
            // try again later in next pass with a larger distance
            if pass < LAST_PASS {
                // LOG_ARPA(wxT(" NOT FOUND IN PASS=%i"), pass);
                // reset what we have done
                pol.time = prev_position.time;
                target.m_refresh_time = prev_refresh;
                target.position = prev_position;
            }
            if target.m_small_fast
                && pass == LAST_PASS - 1
                && target.m_status == TargetStatus::Acquire2
            {
                // status 2, as it was not found,status was not increased.
                // small and fast targets MUST be found in the third sweep, and on a small distance, that is in pass 1.
                log::debug!("smallandfast set lost id={}", target.m_target_id);
                target.set_status_lost();
                target.m_refreshed = RefreshState::OutOfScope;
                return Some(target);
            }

            // delete low status targets immediately when not found
            if ((target.m_status == TargetStatus::Acquire1
                || target.m_status == TargetStatus::Acquire2)
                && pass == LAST_PASS)
                || target.m_status == TargetStatus::Acquire0
            {
                log::debug!(
                    "low status deleted id={}, angle={}, r={}, pass={}, lost_count={}",
                    target.m_target_id,
                    pol.angle,
                    pol.r,
                    pass,
                    target.m_lost_count
                );
                target.set_status_lost();
                target.m_refreshed = RefreshState::OutOfScope;
                return Some(target);
            }
            if pass == LAST_PASS {
                target.m_lost_count += 1;
            }

            // delete if not found too often
            if target.m_lost_count > MAX_LOST_COUNT {
                target.set_status_lost();
                target.m_refreshed = RefreshState::OutOfScope;
                return Some(target);
            }
            target.m_refreshed = RefreshState::NotFound;
            // Send RATTM message also for not seen messages
            /*
            if (pass == LAST_PASS && m_status > m_ri.m_target_age_to_mixer.GetValue()) {
                pol = Pos2Polar(self.position, own_pos);
                if (m_status >= m_ri.m_target_age_to_mixer.GetValue()) {
                    //   f64 dist2target = pol.r / self.pixels_per_meter;
                    LOG_ARPA(wxT(" pass not found as AIVDM targetid=%i"), m_target_id);
                    if (m_transferred_target) {
                        //  LOG_ARPA(wxT(" passTTM targetid=%i"), m_target_id);
                        //  f64 s1 = self.position.dlat_dt;                                   // m per second
                        //  f64 s2 = self.position.dlon_dt;                                   // m  per second
                        //  m_course = rad2deg(atan2(s2, s1));
                        //  PassTTMtoOCPN(&pol, s);

                        PassAIVDMtoOCPN(&pol);
                    }
                    // MakeAndTransmitTargetMessage();
                    // MakeAndTransmitCoT();
                }
            }
            */
            target.m_transferred_target = false;
        } // end of target not found
        return Some(target);
    }

    /// Count the number of pixels in the target, and the number of approaching and receding pixels
    ///
    /// It works by moving outwards from all borders of the target until there is no target pixel
    /// at that radius. On the outside of the target this should only count 1, but on the inside
    /// it will count all pixels in the sweep until it hits the outside. The number of pixels
    /// is not fully correct: the outside pixels are counted twice.
    ///
    fn pixel_counter(&mut self, buffer: &TargetBuffer) {
        //  Counts total number of the various pixels in a blob
        self.m_total_pix = 0;
        self.m_approaching_pix = 0;
        self.m_receding_pix = 0;
        for i in 0..self.contour.contour.len() {
            for radius in self.contour.contour[i].r..buffer.setup.spoke_len {
                let byte = buffer.history
                    [buffer.mod_spokes(self.contour.contour[i].angle) as usize]
                    .sweep[radius as usize];
                let bit0 = (byte & 0x80) != 0; // above threshold bit
                if !bit0 {
                    break;
                }
                // bit1 = (byte & 0x40) >> 6;  // backup bit does not get cleared when target is refreshed
                let bit2 = (byte & 0x20) != 0; // this is Doppler approaching bit
                let bit3 = (byte & 0x10) != 0; // this is Doppler receding bit
                self.m_total_pix += bit0 as u32;
                self.m_approaching_pix += bit2 as u32;
                self.m_receding_pix += bit3 as u32;
            }
        }
    }

    /// Check doppler state of targets if Doppler is on
    fn state_transition(&mut self) {
        if !self.have_doppler || self.m_doppler_target == Doppler::ANY_PLUS {
            return;
        }

        let check_to_doppler = (self.m_total_pix as f64 * 0.85) as u32;
        let check_not_approaching =
            ((self.m_total_pix - self.m_approaching_pix) as f64 * 0.80) as u32;
        let check_not_receding = ((self.m_total_pix - self.m_receding_pix) as f64 * 0.80) as u32;

        let new = match self.m_doppler_target {
            Doppler::ANY_DOPPLER | Doppler::ANY => {
                // convert to APPROACHING or RECEDING
                if self.m_approaching_pix > self.m_receding_pix
                    && self.m_approaching_pix > check_to_doppler
                {
                    &Doppler::APPROACHING
                } else if self.m_receding_pix > self.m_approaching_pix
                    && self.m_receding_pix > check_to_doppler
                {
                    &Doppler::RECEDING
                } else if self.m_doppler_target == Doppler::ANY_DOPPLER {
                    &Doppler::ANY
                } else {
                    &self.m_doppler_target
                }
            }

            Doppler::RECEDING => {
                if self.m_receding_pix < check_not_approaching {
                    &Doppler::ANY
                } else {
                    &self.m_doppler_target
                }
            }

            Doppler::APPROACHING => {
                if self.m_approaching_pix < check_not_receding {
                    &Doppler::ANY
                } else {
                    &self.m_doppler_target
                }
            }
            _ => &self.m_doppler_target,
        };
        if *new != self.m_doppler_target {
            log::debug!(
                "Target {} Doppler state changed from {:?} to {:?}",
                self.m_target_id,
                self.m_doppler_target,
                new
            );
            self.m_doppler_target = *new;
        }
    }

    /*
        void ArpaTarget::TransferTargetToOtherRadar() {
          RadarInfo* other_radar = 0;
          LOG_ARPA(wxT("%s: TransferTargetToOtherRadar m_target_id=%i,"), m_ri.m_name, m_target_id);
          if (M_SETTINGS.radar_count != 2) {
            return;
          }
          if (!m_pi.m_radar[0] || !m_pi.m_radar[1] || !m_pi.m_radar[0].m_arpa || !m_pi.m_radar[1].m_arpa) {
            return;
          }
          if (m_pi.m_radar[0].m_state.GetValue() != RADAR_TRANSMIT || m_pi.m_radar[1].m_state.GetValue() != RADAR_TRANSMIT) {
            return;
          }
          LOG_ARPA(wxT("%s: this  radar pix/m=%f"), m_ri.m_name, self.pixels_per_meter);
          RadarInfo* long_range = m_pi.GetLongRangeRadar();
          RadarInfo* short_range = m_pi.GetShortRangeRadar();

          if (m_ri == long_range) {
            other_radar = short_range;
            int border = (int)(m_ri.m_spoke_len_max * self.pixels_per_meter / short_range.m_pixels_per_meter);
            // m_ri has largest range, other_radar smaller range. Don't transfer targets that are outside range of smaller radar
            if (m_expected.r > border) {
              // don't send small range targets to smaller radar
              return;
            }
          } else {
            other_radar = long_range;
            // this (m_ri) is the small range radar
            // we will only send larger range targets to other radar
          }
          DynamicTargetData data;
          data.target_id = m_target_id;
          data.P = m_kalman.P;
          data.position = self.position;
          LOG_ARPA(wxT("%s: lat= %f, lon= %f, m_target_id=%i,"), m_ri.m_name, self.position.pos.lat, self.position.pos.lon, m_target_id);
          data.status = m_status;
          other_radar.m_arpa.InsertOrUpdateTargetFromOtherRadar(&data, false);
        }

        void ArpaTarget::SendTargetToNearbyRadar() {
          LOG_ARPA(wxT("%s: Send target to nearby radar, m_target_id=%i,"), m_ri.m_name, m_target_id);
          RadarInfo* long_radar = m_pi.GetLongRangeRadar();
          if (m_ri != long_radar) {
            return;
          }
          DynamicTargetData data;
          data.target_id = m_target_id;
          data.P = m_kalman.P;
          data.position = self.position;
          LOG_ARPA(wxT("%s: lat= %f, lon= %f, m_target_id=%i,"), m_ri.m_name, self.position.pos.lat, self.position.pos.lon, m_target_id);
          data.status = m_status;
          if (m_pi.m_inter_radar) {
            m_pi.m_inter_radar.SendTarget(data);
            LOG_ARPA(wxT(" %s, target data send id=%i"), m_ri.m_name, m_target_id);
          }
        }

    */

    fn get_target(&mut self, buffer: &mut TargetBuffer, pol: &mut Polar, dist1: i32) -> bool {
        // general target refresh

        let dist = min(dist1, pol.r - 5);
        let backup_angle = pol.angle;
        let backup_r = pol.r;
        let backup_contour_length = self.contour.length;

        let a = pol.angle;
        let r = pol.r;

        let contour_found = if buffer.pix(&self.m_doppler_target, a, r) {
            buffer.find_contour_from_inside(&self.m_doppler_target, pol)
        } else {
            buffer.find_nearest_contour(&self.m_doppler_target, pol, dist)
        };
        if !contour_found {
            pol.angle = backup_angle;
            pol.r = backup_r;
            self.contour.length = backup_contour_length;
            return false;
        }
        let cont = self.get_contour(&buffer, pol);
        if cont != 0 {
            log::debug!("ARPA contour error {} at {}, {}", cont, a, r,);
            // reset pol in case of error
            pol.angle = backup_angle;
            pol.r = backup_r;
            self.contour.length = backup_contour_length;
            return false;
        }
        return true;
    }

    /*
    void ArpaTarget::PassAIVDMtoOCPN(Polar* pol) {
      if (!m_ri.m_AIVDMtoO.GetValue()) return;
      wxString s_TargID, s_Bear_Unit, s_Course_Unit;
      wxString s_speed, s_course, s_Dist_Unit, s_status;
      wxString s_bearing;
      wxString s_distance;
      wxString s_target_name;
      wxString nmea;

      if (m_status == LOST) return;  // AIS has no "status lost" message
      s_Bear_Unit = wxEmptyString;   // Bearing Units  R or empty
      s_Course_Unit = wxT("T");      // Course type R; Realtive T; true
      s_Dist_Unit = wxT("N");        // Speed/Distance Unit K, N, S N= NM/h = Knots

      // f64 dist = pol.r / self.pixels_per_meter / 1852.;
      f64 bearing = pol.angle * 360. / m_ri.m_spokes;
      if (bearing < 0) bearing += 360;

      int mmsi = m_target_id % 1000000;
      GeoPosition radar_pos;
      m_ri.GetRadarPosition(&radar_pos);
      f64 target_lat, target_lon;

      target_lat = self.position.pos.lat;
      target_lon = self.position.pos.lon;
      wxString result = EncodeAIVDM(mmsi, self.position.speed_kn, target_lon, target_lat, m_course);
      PushNMEABuffer(result);
      m_pi.SendToTargetMixer(result);
    }

    void ArpaTarget::PassTTMtoOCPN(Polar* pol, OCPN_target_status status) {
      // if (!m_ri.m_TTMtoO.GetValue()) return;  // also remove from conf file
      wxString s_TargID, s_Bear_Unit, s_Course_Unit;
      wxString s_speed, s_course, s_Dist_Unit, s_status;
      wxString s_bearing;
      wxString s_distance;
      wxString s_target_name;
      wxString nmea;
      char sentence[90];
      char checksum = 0;
      char* p;
      s_Bear_Unit = wxEmptyString;  // Bearing Units  R or empty
      s_Course_Unit = wxT("T");     // Course type R; Realtive T; true
      s_Dist_Unit = wxT("N");       // Speed/Distance Unit K, N, S N= NM/h = Knots
      // switch (status) {
      // case Q:
      //   s_status = wxT("Q");  // yellow
      //   break;
      // case T:
      //   s_status = wxT("T");  // green
      //   break;
      // case L:
      //   LOG_ARPA(wxT(" id=%i, status == lost"), m_target_id);
      //   s_status = wxT("L");  // ?
      //   break;
      // }

      if (m_doppler_target == ANY) {
        s_status = wxT("Q");  // yellow
      } else {
        s_status = wxT("T");
      }

      f64 dist = pol.r / self.pixels_per_meter / 1852.;
      f64 bearing = pol.angle * 360. / m_ri.m_spokes;

      if (bearing < 0) bearing += 360;
      s_TargID = wxString::Format(wxT("%2i"), m_target_id);
      s_speed = wxString::Format(wxT("%4.2f"), self.position.speed_kn);
      s_course = wxString::Format(wxT("%3.1f"), m_course);
      if (m_automatic) {
        s_target_name = wxString::Format(wxT("ARPA%2i"), m_target_id);
      } else {
        s_target_name = wxString::Format(wxT("MARPA%2i"), m_target_id);
      }
      s_distance = wxString::Format(wxT("%f"), dist);
      s_bearing = wxString::Format(wxT("%f"), bearing);

      /* Code for TTM follows. Send speed and course using TTM*/
      snprintf(sentence, sizeof(sentence), "RATTM,%2s,%s,%s,%s,%s,%s,%s, , ,%s,%s,%s, ",
               (const char*)s_TargID.mb_str(),       // 1 target id
               (const char*)s_distance.mb_str(),     // 2 Targ distance
               (const char*)s_bearing.mb_str(),      // 3 Bearing fr own ship.
               (const char*)s_Bear_Unit.mb_str(),    // 4 Brearing unit ( T = true)
               (const char*)s_speed.mb_str(),        // 5 Target speed
               (const char*)s_course.mb_str(),       // 6 Target Course.
               (const char*)s_Course_Unit.mb_str(),  // 7 Course ref T // 8 CPA Not used // 9 TCPA Not used
               (const char*)s_Dist_Unit.mb_str(),    // 10 S/D Unit N = knots/Nm
               (const char*)s_target_name.mb_str(),  // 11 Target name
               (const char*)s_status.mb_str());      // 12 Target Status L/Q/T // 13 Ref N/A

      for (p = sentence; *p; p++) {
        checksum ^= *p;
      }
      nmea.Printf(wxT("$%s*%02X\r\n"), sentence, (unsigned)checksum);
      LOG_ARPA(wxT("%s: send TTM, target=%i string=%s"), m_ri.m_name, m_target_id, nmea);
      PushNMEABuffer(nmea);
    }

    #define COPYTOMESSAGE(xxx, bitsize)                   \
      for (int i = 0; i < bitsize; i++) {                 \
        bitmessage[index - i - 1] = xxx[bitsize - i - 1]; \
      }                                                   \
      index -= bitsize;

    wxString ArpaTarget::EncodeAIVDM(int mmsi, f64 speed, f64 lon, f64 lat, f64 course) {
      // For encoding !AIVDM type 1 messages following the spec in https://gpsd.gitlab.io/gpsd/AIVDM.html
      // Sender is ecnoded as AI. There is no official identification for radar targets.

      bitset<168> bitmessage;
      int index = 168;
      bitset<6> type(1);  // 6
      COPYTOMESSAGE(type, 6);
      bitset<2> repeat(0);  // 8
      COPYTOMESSAGE(repeat, 2);
      bitset<30> mmsix(mmsi);  // 38
      COPYTOMESSAGE(mmsix, 30);
      bitset<4> navstatus(0);  // under way using engine    // 42
      COPYTOMESSAGE(navstatus, 4);
      bitset<8> rot(0);  // not turning                  // 50
      COPYTOMESSAGE(rot, 8);
      bitset<10> speedx(round(speed * 10));  // 60
      COPYTOMESSAGE(speedx, 10);
      bitset<1> accuracy(0);  // 61
      COPYTOMESSAGE(accuracy, 1);
      bitset<28> lonx(round(lon * 600000));  // 89
      COPYTOMESSAGE(lonx, 28);
      bitset<27> latx(round(lat * 600000));  // 116
      COPYTOMESSAGE(latx, 27);
      bitset<12> coursex(round(course * 10));  // COG       // 128
      COPYTOMESSAGE(coursex, 12);
      bitset<9> true_heading(511);  // 137
      COPYTOMESSAGE(true_heading, 9);
      bitset<6> timestamp(60);  // 60 means not available   // 143
      COPYTOMESSAGE(timestamp, 6);
      bitset<2> maneuvre(0);  // 145
      COPYTOMESSAGE(maneuvre, 2);
      bitset<3> spare;  // 148
      COPYTOMESSAGE(spare, 3);
      bitset<1> flags(0);  // 149
      COPYTOMESSAGE(flags, 1);
      bitset<19> rstatus(0);  // 168
      COPYTOMESSAGE(rstatus, 19);
      wxString AIVDM = "AIVDM,1,1,,A,";
      bitset<6> char_data;
      uint8_t character;
      for (int i = 168; i > 0; i -= 6) {
        for (int j = 0; j < 6; j++) {
          char_data[j] = bitmessage[i - 6 + j];
        }
        character = (uint8_t)char_data.to_ulong();
        if (character > 39) character += 8;
        character += 48;
        AIVDM += character;
      }
      AIVDM += ",0";
      // calculate checksum
      char checks = 0;
      for (size_t i = 0; i < AIVDM.length(); i++) {
        checks ^= (char)AIVDM[i];
      }
      AIVDM.Printf(wxT("!%s*%02X\r\n"), AIVDM, (unsigned)checks);
      LOG_ARPA(wxT("%s: AIS length=%i, string=%s"), m_ri.m_name, AIVDM.length(), AIVDM);
      return AIVDM;
    }

    void ArpaTarget::MakeAndTransmitCoT() {  // currently not used, CoT messages for WinTak are made bij the Targetmixer
      int mmsi = m_target_id;
      wxString mmsi_string;                                    // uid="MMSI - 001000001"
      mmsi_string.Printf(wxT(" uid=\"RADAR - %09i\""), mmsi);  // uid="MMSI - 001000002"
      wxString short_mmsi_string;
      short_mmsi_string.Printf(wxT("\"%09i\""), mmsi);
      wxDateTime dt(self.position.time);
      int year = dt.GetYear();
      int month = dt.GetMonth();
      int day = dt.GetDay();
      int hour = dt.GetHour();
      int minute = dt.GetMinute();
      int second = dt.GetSecond();
      int millisecond = dt.GetMillisecond();
      wxString speed_string;
      speed_string.Printf(wxT("\"%f\""), self.position.speed_kn);
      wxString course_string;
      course_string.Printf(wxT("\"%f\""), m_course);
    #define LIFE 4

      wxString date_time_string;        // "2022-11-12T09:29:34.784
      wxString date_time_string_stale;  // "2022-11-12T09:29:34.784
      date_time_string.Printf(wxT("\"%04i-%02i-%02iT%02i:%02i:%02i.%03i"), year, month, day, hour, minute, second, millisecond);
      date_time_string_stale.Printf(wxT("\"%04i-%02i-%02iT%02i:%02i:%02i.%03i"), year, month, day, hour, minute, second + LIFE,
                                    millisecond);
      wxString long_date_time_string;  //  time="2022-11-12T09:29:34.784000Z" start="2022-11-12T09:29:34.784000Z"
                                       //  stale="2022-11-12T09:29:34.784000Z"
      // start must be later than time, stale later than start
      long_date_time_string =
          " time=" + date_time_string + "000Z\" start=" + date_time_string + "100Z\" stale=" + date_time_string_stale + "000Z\"";
      wxString position_string;  // lat="53.461441" lon="6.178049"
      position_string.Printf(wxT(" lat=\"%f\" lon=\"%f\""), self.position.pos.lat, self.position.pos.lon);
      wxString version_string = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" ?>";

      wxString CoTxml;
      CoTxml = version_string;
      CoTxml += "<event version=\"2.0\" type=\"a-u-S\"" + mmsi_string + " how=\"m-g\"" + long_date_time_string + ">";
      CoTxml += "<point" + position_string + " hae=\"9.0\" le=\"9.0\" ce=\"9.0\" />";
      CoTxml += "<detail" + mmsi_string + ">";
      CoTxml += "<track course=" + course_string + " speed=" + speed_string + " />";
      // CoTxml += "<contact callsign=" + short_mmsi_string + " />";
      // Remarks is not required
      // CoTxml += "<remarks>Country: Netherlands(Kingdom of the) Type : 1 MMSI : 244730001 aiscot@kees-m14.verruijt.lan</remarks>";
      CoTxml += "</detail>";
      // < _aiscot_ is not required
      // CoTxml += "<_aiscot_ cot_host_id = \"aiscot@kees-m14.verruijt.lan\" country=\"Netherlands (Kingdom of the)\" type=\"1\"
      // mmsi=\"244730002\" aton=\"False\" uscg=\"False\" crs=\"False\" />";
      CoTxml += "</event>";
      LOG_ARPA(wxT("%s: COTxml=\n%s"), m_ri.m_name, CoTxml);
      m_pi.SendToTargetMixer(CoTxml);
    }

    void ArpaTarget::MakeAndTransmitTargetMessage() {
      /* Example message
      {"target":{"uid":1,"lat":52.038339,"lon":4.111908,"sog": 2.08,"cog":39.3,
      "time":"2022-12-29T15:38:11.307000Z","stale":"2022-12-29T15:38:15.307000Z","state":5,
      "lost_count":0}}

      */

    #define LIFE_TIME_SEC 3 // life time of target after last refresh in seconds

  wxString message = wxT("{\"target\":{");

  message << wxT("\"uid\":");
  message << m_target_id;

  message << wxT(",\"source_id\":");
  message << m_pi.m_radar_id;

  message << wxT(",\"lat\":");
  message << self.position.pos.lat;
  message << wxT(",\"lon\":");
  message << self.position.pos.lon;

  wxDateTime dt(self.position.time);
  dt = dt.ToUTC();
  message << wxT(",\"time\":\"");
  message << dt.FormatISOCombined() << wxString::Format(wxT(".%03uZ\""), dt.GetMillisecond(wxDateTime::TZ::GMT0));

  dt += wxTimeSpan(0, 0, LIFE_TIME_SEC, 0);
  message << wxT(",\"stale\":\"");
  message << dt.FormatISOCombined() << wxString::Format(wxT(".%03uZ\""), dt.GetMillisecond(wxDateTime::TZ::GMT0));

  message << wxString::Format(wxT(",\"sog\":%5.2f"), self.position.speed_kn * 1852. / 3600.);
  message << wxString::Format(wxT(",\"cog\":%4.1f"), m_course);

  message << wxT(",\"state\":");
  message << m_status;
  message << wxT(",\"lost_count\":");
  message << m_lost_count;

  message << wxT("}}");

  m_pi.SendToTargetMixer(message);
}
  */

    fn set_status_lost(&mut self) {
        self.contour.length = 0;
        self.m_previous_contour_length = 0;
        self.m_lost_count = 0;
        self.m_kalman.reset_filter();
        self.m_status = TargetStatus::LOST;
        self.m_automatic = false;
        self.m_refresh_time = 0;
        self.m_course = 0.;
        self.m_stationary = 0;
        self.position.dlat_dt = 0.;
        self.position.dlon_dt = 0.;
        self.position.speed_kn = 0.;
    }

    // resets the pixels of the current blob (plus DISTANCE_BETWEEN_TARGETS) so that blob will not be found again in the same sweep
    // We not only reset the blob but all pixels in a radial "square" covering the blob
    fn reset_pixels(&self, buffer: &mut TargetBuffer) {
        const DISTANCE_BETWEEN_TARGETS: i32 = 30;
        const SHADOW_MARGIN: i32 = 5;
        const TARGET_DISTANCE_FOR_BLANKING_SHADOW: f64 = 6000.; // 6 km

        let setup = &buffer.setup;

        for a in self.contour.min_angle - DISTANCE_BETWEEN_TARGETS
            ..=self.contour.max_angle + DISTANCE_BETWEEN_TARGETS
        {
            let a = buffer.mod_spokes(a) as usize;
            for r in max(self.contour.min_r - DISTANCE_BETWEEN_TARGETS, 0)
                ..=min(
                    self.contour.max_r + DISTANCE_BETWEEN_TARGETS,
                    setup.spoke_len - 1,
                )
            {
                buffer.history[a].sweep[r as usize] &= 0x40; // also clear both Doppler bits
            }
        }

        let distance_to_radar = self.m_polar_pos.r as f64 / setup.pixels_per_meter;
        // For larger targets clear the "shadow" of the target until 4 * r ????
        if self.contour.length > 20 && distance_to_radar < TARGET_DISTANCE_FOR_BLANKING_SHADOW {
            let mut max = self.contour.max_angle;
            if self.contour.min_angle - SHADOW_MARGIN > self.contour.max_angle + SHADOW_MARGIN {
                max += setup.spokes;
            }
            for a in self.contour.min_angle - SHADOW_MARGIN..=max + SHADOW_MARGIN {
                let a = buffer.mod_spokes(a) as usize;
                for r in self.contour.max_r..=min(4 * self.contour.max_r, setup.spoke_len - 1) {
                    buffer.history[a].sweep[r as usize] &= 0x40;
                    // also clear both Doppler bits
                }
            }
        }
    }
}
