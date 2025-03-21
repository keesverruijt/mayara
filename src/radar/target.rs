use std::{
    cell::RefCell,
    cmp::{max, min},
    collections::HashMap,
    f64::consts::PI,
    rc::Rc,
};

use arpa::ArpaTarget;
use kalman::Polar;
use ndarray::Array2;

use super::GeoPosition;

mod arpa;
mod kalman;

const MIN_CONTOUR_LENGTH: usize = 6;
const MAX_CONTOUR_LENGTH: usize = 2000; // defines maximal size of target contour in pixels
const MAX_LOST_COUNT: i32 = 12; // number of sweeps that target can be missed before it is set to lost
const MAX_DETECTION_SPEED_KN: f64 = 40.;

const METERS_PER_DEGREE_LATITUDE: f64 = 60. * 1852.;
const KN_TO_MS: f64 = 1852. / 3600.;

const TODO_ROTATION_SPEED_MS: i32 = 2500;
const TODO_TARGET_AGE_TO_MIXER: u32 = 5;

///
/// The length of a degree longitude varies by the latitude,
/// the more north or south you get the shorter it becomes.
/// Since the earth is _nearly_ a sphere, the cosine function
/// is _very_ close.
///
pub fn meters_per_degree_longitude(lat: &f64) -> f64 {
    METERS_PER_DEGREE_LATITUDE * lat.to_radians().cos()
}

#[derive(Debug, Clone)]
struct ExtendedPosition {
    pos: GeoPosition,
    dlat_dt: f64, // m / sec
    dlon_dt: f64, // m / sec
    time: u64,    // millis
    speed_kn: f64,
    sd_speed_kn: f64, // standard deviation of the speed in knots
}

impl ExtendedPosition {
    fn new(
        pos: GeoPosition,
        dlat_dt: f64,
        dlon_dt: f64,
        time: u64,
        speed_kn: f64,
        sd_speed_kn: f64,
    ) -> Self {
        Self {
            pos,
            dlat_dt,
            dlon_dt,
            time,
            speed_kn,
            sd_speed_kn,
        }
    }
    fn empty() -> Self {
        Self::new(GeoPosition::new(0., 0.), 0., 0., 0, 0., 0.)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum TargetStatus {
    Acquire0,    // Under acquisition, first seen, no contour yet
    Acquire1,    // Under acquisition, first contour found
    Acquire2,    // Under acquisition, speed and course known
    ACQUIRE3,    // Under acquisition, speed and course known, next time active
    ACTIVE,      // Active target
    LOST,        // Lost target
    ForDeletion, // Target to be deleted
}

#[derive(Debug, Clone, PartialEq)]
enum RefreshState {
    NotFound,
    Found,
    OutOfScope,
}

/*
Doppler states of the target.
A Doppler state of a target is an attribute of the target that determines the
search method for the target in the history array, according to the following
table:

x means don't care, bit0 is the above threshold bit, bit2 is the APPROACHING
bit, bit3 is the RECEDING bit.

                  bit0   bit2   bit3
ANY                  1      x      x
NO_DOPPLER           1      0      0
APPROACHING          1      1      0
RECEDING             1      0      1
ANY_DOPPLER          1      1      0   or
                     1      0      1
NOT_RECEDING         1      x      0
NOT_APPROACHING      1      0      x

ANY is typical non Dopper target
NOT_RECEDING and NOT_APPROACHING are only used to check countour length in the
transition of APPROACHING or RECEDING -> ANY ANY_DOPPLER is only used in the
search for targets and converted to APPROACHING or RECEDING in the first refresh
cycle

State transitions:
ANY -> APPROACHING or RECEDING  (not yet implemented)
APPROACHING or RECEDING -> ANY  (based on length of contours)
ANY_DOPPLER -> APPROACHING or RECEDING

*/
#[derive(Debug, Clone, Copy, PartialEq)]
enum Doppler {
    ANY,        // any target above threshold
    NO_DOPPLER, // a target without a Doppler bit
    APPROACHING,
    RECEDING,
    ANY_DOPPLER,     // APPROACHING or RECEDING
    NOT_RECEDING,    // that is NO_DOPPLER or APPROACHING
    NOT_APPROACHING, // that is NO_DOPPLER or RECEDING
    ANY_PLUS,        // will also check bits that have been cleared
}

const FOUR_DIRECTIONS: [Polar; 4] = [
    Polar {
        angle: 0,
        r: 1,
        time: 0,
    },
    Polar {
        angle: 1,
        r: 0,
        time: 0,
    },
    Polar {
        angle: 0,
        r: -1,
        time: 0,
    },
    Polar {
        angle: -1,
        r: 0,
        time: 0,
    },
];

#[derive(Debug, Clone)]
struct HistorySpoke {
    sweep: Vec<u8>,
    time: u64,
    pos: GeoPosition,
}

impl HistorySpoke {
    fn new(sweep: Vec<u8>, time: u64, pos: GeoPosition) -> Self {
        Self { sweep, time, pos }
    }

    fn image(spokes: i32, spoke_len: i32) -> Vec<HistorySpoke> {
        vec![
            HistorySpoke::new(vec![0; spoke_len as usize], 0, GeoPosition::new(0., 0.));
            spokes as usize
        ]
    }
}

#[derive(Debug, Clone)]
pub(self) struct TargetSetup {
    radar_id: u32,
    spokes: i32,
    spokes_f64: f64,
    spoke_len: i32,
    have_doppler: bool,
    pixels_per_meter: f64,
}

#[derive(Debug, Clone)]
pub struct TargetBuffer {
    setup: TargetSetup,
    next_target_id: u32,
    stationary_layer: Option<Box<Array2<u8>>>,
    history: Box<Vec<HistorySpoke>>,
    targets: Rc<RefCell<HashMap<u32, ArpaTarget>>>,
    // wxLongLong m_doppler_arpa_update_time[SPOKES_MAX];
    m_clear_contours: bool,
    m_auto_learn_state: i32,
    // std::deque<GeoPosition> m_delete_target_position;
    // std::deque<DynamicTargetData*> m_remote_target_queue;
}

impl TargetBuffer {
    pub fn new(
        radar_id: u32,
        stationary: bool,
        spokes: i32,
        spoke_len: i32,
        have_doppler: bool,
    ) -> Self {
        TargetBuffer {
            setup: TargetSetup {
                radar_id,
                spokes,
                spokes_f64: spokes as f64,
                spoke_len,
                have_doppler,
                pixels_per_meter: 0.0,
            },
            next_target_id: 0,
            stationary_layer: if stationary {
                Some(Box::new(Array2::<u8>::zeros((
                    spokes as usize,
                    spoke_len as usize,
                ))))
            } else {
                None
            },
            history: Box::new(HistorySpoke::image(spokes, spoke_len)),
            targets: Rc::new(RefCell::new(HashMap::new())),
            m_clear_contours: false,
            m_auto_learn_state: 0,
        }
    }

    pub fn set_pixels_per_meter(&mut self, pixels_per_meter: f64) {
        self.setup.pixels_per_meter = pixels_per_meter;
    }

    fn get_next_target_id(&mut self) -> u32 {
        const MAX_TARGET_ID: u32 = 100000;

        self.next_target_id += 1;
        if self.next_target_id >= MAX_TARGET_ID {
            self.next_target_id = 1;
        }

        self.next_target_id + MAX_TARGET_ID * self.setup.radar_id
    }

    pub fn mod_spokes(&self, angle: i32) -> i32 {
        (angle + self.setup.spokes) % self.setup.spokes
    }

    /// Number of sweeps that a next scan of the target may have moved, 1/10th of circle
    pub fn scan_margin(&self) -> i32 {
        self.setup.spokes / 10
    }

    /// Number of sweeps that indicate we're on the next rotation
    pub fn scan_next_rotation(&self) -> i32 {
        self.setup.spokes / 2
    }

    pub fn pix(&self, doppler: &Doppler, ang: i32, rad: i32) -> bool {
        if rad <= 0 || rad >= self.setup.spoke_len as i32 {
            return false;
        }
        let angle = self.mod_spokes(ang);

        if let Some(layer) = &self.stationary_layer {
            if layer[[angle as usize, rad as usize]] != 0 {
                return false;
            }
        }
        let history = self.history[angle as usize].sweep[rad as usize];
        let bit0 = (history & 0x80) != 0; // above threshold bit
        let bit1 = (history & 0x40) != 0; // backup bit does not get cleared when target is refreshed
        let bit2 = (history & 0x20) != 0; // this is Doppler approaching bit
        let bit3 = (history & 0x10) != 0; // this is Doppler receding bit

        match doppler {
            Doppler::ANY => bit0,
            Doppler::NO_DOPPLER => bit0 && !bit2 && !bit3,
            Doppler::APPROACHING => bit2,
            Doppler::RECEDING => bit3,
            Doppler::ANY_DOPPLER => bit2 || bit3,
            Doppler::NOT_RECEDING => bit0 && !bit3,
            Doppler::NOT_APPROACHING => bit0 && !bit2,
            Doppler::ANY_PLUS => bit1,
        }
    }

    fn multi_pix(&mut self, doppler: &Doppler, ang: i32, rad: i32) -> bool {
        // checks if the blob has a contour of at least length pixels
        // pol must start on the contour of the blob
        // false if not
        // if false clears out pixels of the blob in hist

        if !self.pix(doppler, ang, rad) {
            return false;
        }
        let length = MIN_CONTOUR_LENGTH;
        let start = Polar::new(ang as i32, rad as i32, 0);
        if start.r >= self.setup.spoke_len {
            return false; //  r too large
        }
        if start.r < 3 {
            return false; //  r too small
        }

        let mut current = start; // the 4 possible translations to move from a point on the contour to the next

        let mut max_angle = current;
        let mut min_angle = current;
        let mut max_r = current;
        let mut min_r = current;
        let mut count = 0;
        let mut found = false;

        // first find the orientation of border point p
        let index = {
            let mut index = 0;
            for i in 0..4 {
                if !self.pix(
                    doppler,
                    current.angle + FOUR_DIRECTIONS[i].angle,
                    current.r + FOUR_DIRECTIONS[i].r,
                ) {
                    found = true;
                    break;
                }
                index += 1;
            }
            if !found {
                return false; // single pixel blob
            }
            index
        };
        let mut index = (index + 1) % 4; // determines starting direction
        found = false;

        while current.r != start.r || current.angle != start.angle || count == 0 {
            // try all translations to find the next point
            // start with the "left most" translation relative to the
            // previous one
            index = (index + 3) % 4; // we will turn left all the time if possible
            for _ in 0..4 {
                if self.pix(
                    doppler,
                    current.angle + FOUR_DIRECTIONS[index].angle,
                    current.r + FOUR_DIRECTIONS[index].r,
                ) {
                    found = true;
                    break;
                }
                index = (index + 1) % 4;
            }
            if !found {
                return false; // no next point found (this happens when the blob consists of one single pixel)
            } // next point found
            current.angle += FOUR_DIRECTIONS[index].angle;
            current.r += FOUR_DIRECTIONS[index].r;
            if count >= length {
                return true;
            }
            count += 1;
            if current.angle > max_angle.angle {
                max_angle = current;
            }
            if current.angle < min_angle.angle {
                min_angle = current;
            }
            if current.r > max_r.r {
                max_r = current;
            }
            if current.r < min_r.r {
                min_r = current;
            }
        } // contour length is less than m_min_contour_length
          // before returning false erase this blob so we do not have to check this one again
        if min_angle.angle < 0 {
            min_angle.angle += self.setup.spokes;
            max_angle.angle += self.setup.spokes;
        }
        for a in min_angle.angle..=max_angle.angle {
            let a_normalized = self.mod_spokes(a) as usize;
            for r in min_r.r..=max_r.r {
                self.history[a_normalized].sweep[r as usize] &= 0x3f;
            }
        }
        return false;
    }

    // moves pol to contour of blob
    // true if success
    // false when failed
    fn find_contour_from_inside(&mut self, doppler: &Doppler, pol: &mut Polar) -> bool {
        let mut ang = pol.angle;
        let rad = pol.r;
        let mut limit = self.setup.spokes;

        if rad >= self.setup.spoke_len || rad < 3 {
            return false;
        }
        if !self.pix(doppler, ang, rad) {
            return false;
        }
        while limit >= 0 && self.pix(doppler, ang, rad) {
            ang -= 1;
            limit -= 1;
        }
        ang += 1;
        pol.angle = ang;

        // return true if the blob has the required min contour length
        self.multi_pix(doppler, ang, rad)
    }

    fn pix2(&mut self, doppler: &Doppler, pol: &mut Polar, a: i32, r: i32) -> bool {
        if r < self.setup.spoke_len - 1 && self.multi_pix(doppler, a, r) {
            pol.angle = a;
            pol.r = r;
            return true;
        }
        return false;
    }

    /// make a search pattern along a square
    /// returns the position of the nearest blob found in pol
    /// dist is search radius (1 more or less) in radial pixels
    fn find_nearest_contour(&mut self, doppler: &Doppler, pol: &mut Polar, dist: i32) -> bool {
        let a = pol.angle;
        let r = pol.r;
        let distance = max(dist, 2);
        let factor: f64 = self.setup.spokes as f64 / 2.0 / PI;

        for j in 1..=distance {
            let dist_r = j;
            let dist_a = max((factor / r as f64 * j as f64) as i32, 1);
            // search starting from the middle
            for i in 0..=dist_a {
                // "upper" side
                if self.pix2(doppler, pol, a - i, r + dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r + dist_r) {
                    return true;
                }
            }
            for i in 0..dist_r {
                // "right hand" side
                if self.pix2(doppler, pol, a + dist_a, r + i) {
                    return true;
                }
                if self.pix2(doppler, pol, a + dist_a, r - i) {
                    return true;
                }
            }
            for i in 0..=dist_a {
                // "lower" side
                if self.pix2(doppler, pol, a - i, r - dist_r) {
                    return true;
                }
                if self.pix2(doppler, pol, a + i, r - dist_r) {
                    return true;
                }
            }
            for i in 0..dist_r {
                // "left hand" side
                if self.pix2(doppler, pol, a - dist_a, r + i) {
                    return true;
                }
                if self.pix2(doppler, pol, a - dist_a, r - i) {
                    return true;
                }
            }
        }
        false
    }

    pub fn polar2pos(&self, pol: &Polar, own_ship: &ExtendedPosition) -> ExtendedPosition {
        // The "own_ship" in the function call can be the position at an earlier time than the current position
        // converts in a radar image angular data r ( 0 - max_spoke_len ) and angle (0 - max_spokes) to position (lat, lon)
        // based on the own ship position own_ship
        let mut pos: ExtendedPosition = own_ship.clone();
        // should be revised, use Mercator formula PositionBearingDistanceMercator()  TODO
        pos.pos.lat += (pol.r as f64 / self.setup.pixels_per_meter)  // Scale to fraction of distance from radar
                                       * pol.angle_in_rad(self.setup.spokes_f64).cos()
            / METERS_PER_DEGREE_LATITUDE;
        pos.pos.lon += (pol.r as f64 / self.setup.pixels_per_meter)  // Scale to fraction of distance to radar
                                       * pol.angle_in_rad(self.setup.spokes_f64).sin()
            / own_ship.pos.lat.to_radians().cos()
            / METERS_PER_DEGREE_LATITUDE;
        pos
    }

    pub fn pos2polar(&self, p: &ExtendedPosition, own_ship: &ExtendedPosition) -> Polar {
        // converts in a radar image a lat-lon position to angular data relative to position own_ship

        let dif_lat = p.pos.lat - own_ship.pos.lat;
        let dif_lon = (p.pos.lon - own_ship.pos.lon) * own_ship.pos.lat.to_radians().cos();
        let r = ((dif_lat * dif_lat + dif_lon * dif_lon).sqrt()
            * METERS_PER_DEGREE_LATITUDE
            * self.setup.pixels_per_meter
            + 1.) as i32;
        let mut angle = f64::atan2(dif_lon, dif_lat) * self.setup.spokes_f64 / (2. * PI) + 1.; // + 1 to minimize rounding errors
        if angle < 0. {
            angle += self.setup.spokes_f64;
        }
        return Polar::new(angle as i32, r, p.time);
    }

    //
    // FUNCTIONS COMING FROM "RadarArpa"
    //

    fn find_target_id_by_position(&self, pos: &GeoPosition) -> Option<u32> {
        let mut best_id = None;
        let mut min_dist = 1000.;
        for (id, target) in self.targets.borrow().iter() {
            if target.m_status != TargetStatus::LOST {
                let dif_lat = pos.lat - target.position.pos.lat;
                let dif_lon = (pos.lon - target.position.pos.lon) * pos.lat.to_radians().cos();
                let dist2 = dif_lat * dif_lat + dif_lon * dif_lon;
                if dist2 < min_dist {
                    min_dist = dist2;
                    best_id = Some(*id);
                }
            }
        }

        best_id
    }

    fn acquire_new_marpa_target(&mut self, target_pos: ExtendedPosition) {
        self.acquire_or_delete_marpa_target(target_pos, TargetStatus::Acquire0);
    }

    /// Delete the target that is closest to the position   
    fn delete_target(&mut self, pos: &GeoPosition) {
        if let Some(id) = self.find_target_id_by_position(pos) {
            self.targets.borrow_mut().remove(&id);
        } else {
            log::debug!(
                "Could not find (M)ARPA target to delete within 1000 meters from {}",
                pos
            );
        }
    }

    fn acquire_or_delete_marpa_target(
        &mut self,
        target_pos: ExtendedPosition,
        status: TargetStatus,
    ) {
        // acquires new target from mouse click position
        // no contour taken yet
        // target status acquire0
        // returns in X metric coordinates of click
        // constructs Kalman filter
        // make new target

        log::debug!("Adding (M)ARPA target at {}", target_pos.pos);

        let id = self.get_next_target_id();
        let target = ArpaTarget::new(
            target_pos,
            id,
            self.setup.spokes as usize,
            status,
            self.setup.have_doppler,
        );
        self.targets.borrow_mut().insert(id, target);
    }

    /*
    void RadarArpa::DrawArpaTargetsPanel(double scale, double arpa_rotate) {
      wxPoint boat_center;
      GeoPosition radar_pos, target_pos;
      double offset_lat = 0.;
      double offset_lon = 0.;

      if (!m_pi.m_settings.drawing_method && m_ri->GetRadarPosition(&radar_pos)) {
        m_ri->GetRadarPosition(&radar_pos);
        for (auto target = m_targets.cbegin(); target != m_targets.cend(); target++) {
          if ((*target).m_status == LOST) {
            continue;
          }
          target_pos = (*target).m_radar_pos;
          offset_lat = (radar_pos.lat - target_pos.lat) * 60. * 1852. * m_ri.m_panel_zoom / m_ri.m_range.GetValue();
          offset_lon = (radar_pos.lon - target_pos.lon) * 60. * 1852. * cos(deg2rad(target_pos.lat)) * m_ri.m_panel_zoom /
                       m_ri.m_range.GetValue();
          glPushMatrix();
          glRotated(arpa_rotate, 0.0, 0.0, 1.0);
          glTranslated(-offset_lon, offset_lat, 0);
          glScaled(scale, scale, 1.);
          DrawContour(target->get());
          glPopMatrix();
        }
      }

      else {
        glPushMatrix();
        glTranslated(0., 0., 0.);
        glRotated(arpa_rotate, 0.0, 0.0, 1.0);
        glScaled(scale, scale, 1.);
        for (auto target = m_targets.cbegin(); target != m_targets.cend(); target++) {
          if ((*target).m_status != LOST) {
            DrawContour(target->get());
          }
        }
        glPopMatrix();
      }
    }
      */

    pub(crate) fn cleanup_lost_targets(&mut self) {
        // remove targets with status LOST
        self.targets
            .borrow_mut()
            .retain(|_, t| t.m_status != TargetStatus::LOST);
        for (_, v) in self.targets.borrow_mut().iter_mut() {
            v.m_refreshed = RefreshState::NotFound;
        }
    }

    pub(crate) fn refresh_all_arpa_targets(&mut self) {
        log::debug!(
            "***refresh loop start m_targets.size={}",
            self.targets.borrow().len()
        );
        if self.setup.pixels_per_meter != 0. {
            self.cleanup_lost_targets();
        }

        // main target refresh loop

        // pass 0 of target refresh  Only search for moving targets faster than 2 knots as long as autolearnng is initializing
        // When autolearn is ready, apply for all targets

        let speed = MAX_DETECTION_SPEED_KN * KN_TO_MS; // m/sec
        let search_radius =
            (speed * TODO_ROTATION_SPEED_MS as f64 * self.setup.pixels_per_meter / 1000.) as i32;
        log::debug!(
            "Search radius={}, pix/m={}",
            search_radius,
            self.setup.pixels_per_meter
        );

        for (_id, target) in self.targets.borrow().iter() {
            if (target.position.speed_kn >= 2.5 && target.age_rotations >= TODO_TARGET_AGE_TO_MIXER)
                || self.m_auto_learn_state >= 1
            {
                let clone = target.clone();
                if let Some(_target) = ArpaTarget::refresh_target(clone, self, search_radius / 4, 0)
                { // was 5
                }
            }
        }

        // pass 1 of target refresh
        #[cfg(Todo)]
        for (_id, target) in self.targets.borrow_mut().iter_mut() {
            target.refresh_target(self, search_radius / 3, 1);
        }

        #[cfg(Todo)]
        for (_id, target) in self.targets.borrow_mut().iter_mut() {
            target.refresh_target(self, search_radius, 2);
        }
    }

    /**
     * Inject the target in this radar.
     *
     * Called from the main thread and from the InterRadar receive thread.
     */
    /*
    void RadarArpa::InsertOrUpdateTargetFromOtherRadar(const DynamicTargetData* data, bool remote) {
      wxCriticalSectionLocker lock(m_ri.m_exclusive);

      // This method works on the other radar than TransferTargetToOtherRadar
      // find target
      bool found = false;
      int uid = data->target_id;
      LOG_ARPA(wxT("%s: InsertOrUpdateTarget id=%i"), m_ri.m_name, uid);
      ArpaTarget* updated_target = 0;
      for (auto target = m_targets.begin(); target != m_targets.end(); target++) {
        //LOG_ARPA(wxT("%s: InsertOrUpdateTarget id=%i, found=%i"), m_ri.m_name, uid, (*target).m_target_id);
        if ((*target).m_target_id == uid) {  // target found!
          updated_target = (*target).get();
          found = true;
          LOG_ARPA(wxT("%s: InsertOrUpdateTarget found target id=%d pos=%d"), m_ri.m_name, uid, target - m_targets.begin());
          break;
        }
      }
      if (!found) {
        // make new target with existing uid
        LOG_ARPA(wxT("%s: InsertOrUpdateTarget new target id=%d, pos=%ld"), m_ri.m_name, uid, m_targets.size());
    #ifdef __WXMSW__
        std::unique_ptr<ArpaTarget> new_target = std::make_unique<ArpaTarget>(m_pi, m_ri, uid);
        #else
        std::unique_ptr<ArpaTarget> new_target = make_unique<ArpaTarget>(m_pi, m_ri, uid);
        #endif
        updated_target = new_target.get();
        m_targets.push_back(std::move(new_target));
        ExtendedPosition own_pos;
        if (remote) {
          m_ri->GetRadarPosition(&own_pos);
          Polar pol = updated_target->Pos2Polar(data->position, own_pos);
          LOG_ARPA(wxT("%s: InsertOrUpdateTarget new target id=%d polar=%i"), m_ri.m_name, uid, pol.angle);
          // set estimated time of last refresh as if it was a local target
          updated_target.m_refresh_time = m_ri.m_history[MOD_SPOKES(pol.angle)].time;
        }
      }
      //LOG_ARPA(wxT("%s: InsertOrUpdateTarget processing id=%i"), m_ri.m_name, uid);
      updated_target.m_kalman.P = data->P;
      updated_target.m_position = data->position;
      updated_target.m_status = data->status;
      LOG_ARPA(wxT("%s: transferred id=%i, lat= %f, lon= %f, status=%i,"), m_ri.m_name, updated_target.m_target_id,
               updated_target.m_position.pos.lat, updated_target.m_position.pos.lon, updated_target.m_status);
      updated_target.m_doppler_target = ANY;
      updated_target.m_lost_count = 0;
      updated_target.m_automatic = true;
      double s1 = updated_target.m_position.dlat_dt;  // m per second
      double s2 = updated_target.m_position.dlon_dt;                                   // m  per second
      updated_target.m_course = rad2deg(atan2(s2, s1));
      if (remote) {   // inserted or updated target originated from another radar
        updated_target.m_transferred_target = true;
        //LOG_ARPA(wxT(" m_transferred_target = true targetid=%i"), updated_target.m_target_id);
      }
      return;
    }
    */

    fn delete_all_targets(&mut self) {
        self.targets.borrow_mut().clear();
    }

    /*

    bool RadarArpa::AcquireNewARPATarget(Polar pol, int status, Doppler doppler) {
      // acquires new target at polar position pol
      // no contour taken yet
      // target status status, normally 0, if dummy target to delete a target -2
      // constructs Kalman filter
      ExtendedPosition own_pos;
      ExtendedPosition target_pos;
      Doppler doppl = doppler;
      if (!m_ri->GetRadarPosition(&own_pos.pos)) {
        return false;
      }

      // make new target
    #ifdef __WXMSW__
      std::unique_ptr<ArpaTarget> target = std::make_unique<ArpaTarget>(m_pi, m_ri, 0);
      #else
      std::unique_ptr<ArpaTarget> target = make_unique<ArpaTarget>(m_pi, m_ri, 0);
      #endif
      target_pos = target->Polar2Pos(pol, own_pos);
      target.m_doppler_target = doppl;
      target.m_position = target_pos;  // Expected position
      target.m_position.time = wxGetUTCTimeMillis();
      target.m_position.dlat_dt = 0.;
      target.m_position.dlon_dt = 0.;
      target.m_position.speed_kn = 0.;
      target.m_position.sd_speed_kn = 0.;
      target.m_status = status;
      target.m_max_angle.angle = 0;
      target.m_min_angle.angle = 0;
      target.m_max_r.r = 0;
      target.m_min_r.r = 0;
      target.m_doppler_target = doppl;
      target.m_refreshed = NOT_FOUND;
      target.m_automatic = true;
      target->RefreshTarget(TARGET_SEARCH_RADIUS1, 1);

      m_targets.push_back(std::move(target));
      return true;
    }

    void RadarArpa::ClearContours() { m_clear_contours = true; }
    */

    /*
    void RadarArpa::ProcessIncomingMessages() {
      DynamicTargetData* target;
      if (m_clear_contours) {
        m_clear_contours = false;
        for (auto target = m_targets.begin(); target != m_targets.end(); target++) {
          (*target).m_contour_length = 0;
          (*target).m_previous_contour_length = 0;
        }
      }

      while ((target = GetIncomingRemoteTarget()) != NULL) {
        InsertOrUpdateTargetFromOtherRadar(target, true);
        delete target;
      }
    }
      */

    /*
    bool RadarArpa::IsAtLeastOneRadarTransmitting() {
      for (size_t r = 0; r < RADARS; r++) {
        if (m_pi.m_radar[r] != NULL && m_pi.m_radar[r].m_state.GetValue() == RADAR_TRANSMIT) {
          return true;
        }
      }
      return false;
    }
      */

    /*
    void RadarArpa::SearchDopplerTargets() {
      ExtendedPosition own_pos;

      if (!m_pi.m_settings.show                       // No radar shown
          || !m_ri->GetRadarPosition(&own_pos.pos)     // No position
          || m_pi->GetHeadingSource() == HEADING_NONE  // No heading
          || (m_pi->GetHeadingSource() == HEADING_FIX_HDM && m_pi.m_var_source == VARIATION_SOURCE_NONE)) {
        return;
      }

      if (m_ri.m_pixels_per_meter == 0. || !IsAtLeastOneRadarTransmitting()) {
        return;
      }

      size_t range_start = 20;  // Convert from meters to 0..511
      size_t range_end;
      int outer_limit = m_ri.m_spoke_len_max;
      outer_limit = (int)outer_limit * 0.93;
      range_end = outer_limit;  // Convert from meters to 0..511

      SpokeBearing start_bearing = 0;
      SpokeBearing end_bearing = m_ri.m_spokes;

      // loop with +2 increments as target must be larger than 2 pixels in width
      for (int angleIter = start_bearing; angleIter < end_bearing; angleIter += 2) {
        SpokeBearing angle = MOD_SPOKES(angleIter);
        wxLongLong angle_time = m_ri.m_history[angle].time;
        // angle_time_plus_margin must be timed later than the pass 2 in refresh, otherwise target may be found multiple times
        wxLongLong angle_time_plus_margin = m_ri.m_history[MOD_SPOKES(angle + 3 * SCAN_MARGIN)].time;

        // check if target has been refreshed since last time
        // and if the beam has passed the target location with SCAN_MARGIN spokes
        if ((angle_time > (m_doppler_arpa_update_time[angle] + SCAN_MARGIN2) &&
             angle_time_plus_margin >= angle_time)) {  // the beam sould have passed our "angle" AND a
                                                       // point SCANMARGIN further set new refresh time
          m_doppler_arpa_update_time[angle] = angle_time;
          for (int rrr = (int)range_start; rrr < (int)range_end; rrr++) {
            if (m_ri.m_arpa->MultiPix(angle, rrr, ANY_DOPPLER)) {
              // pixel found that does not belong to a known target
              Polar pol;
              pol.angle = angle;
              pol.r = rrr;
              if (!m_ri.m_arpa->AcquireNewARPATarget(pol, 0, ANY_DOPPLER)) {
                break;
              }
            }
          }
        }
      }

      return;
    }
    */

    /*
    DynamicTargetData* RadarArpa::GetIncomingRemoteTarget() {
      wxCriticalSectionLocker lock(m_remote_target_lock);
      DynamicTargetData* next;
      if (m_remote_target_queue.empty()) {
        next = NULL;
      } else {
        next = m_remote_target_queue.front();
        m_remote_target_queue.pop_front();
      }
      return next;
    }
    */

    /*
     * Safe to call from any thread
     */
    /*
    void RadarArpa::StoreRemoteTarget(DynamicTargetData* target) {
      wxCriticalSectionLocker lock(m_remote_target_lock);
      m_remote_target_queue.push_back(target);
    }
      */
}
