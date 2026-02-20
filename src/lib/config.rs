use directories::ProjectDirs;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::radar::RadarInfo;
use crate::radar::range::Ranges;

pub fn get_project_dirs() -> ProjectDirs {
    directories::ProjectDirs::from("net", "verruijt", "mayara")
        .expect("Cannot find project directories")
}

fn default_range_units() -> i32 {
    0 // Nautical (default)
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Radar {
    pub id: usize,
    pub user_name: String,
    #[serde(default)]
    pub spoke_processing: i32, // 0 = Clean, 1 = Smoothing
    #[serde(default = "default_range_units")]
    pub range_units: i32, // 0 = Nautical, 1 = Metric, 2 = Mixed

    // Data that is computed and not immediately known when starting
    pub model_name: Option<String>, // Descriptive model name (4G, HALO)
    pub ranges: Option<Vec<i32>>,   // Detected ranges
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct Config {
    pub radars: HashMap<String, Radar>,
}

#[derive(Debug, Clone)]
pub(crate) struct Persistence {
    pub config: Config,
    timestamp: SystemTime,
    path: PathBuf,
}

impl Persistence {
    pub fn new() -> Self {
        let project_dirs = get_project_dirs();
        let mut settings_path = project_dirs.config_dir().to_owned();
        fs::create_dir_all(&settings_path).expect("Cannot create settings directory");
        settings_path.push("settings.json");

        let mut this = Persistence {
            config: Config {
                radars: HashMap::new(),
            },
            timestamp: SystemTime::UNIX_EPOCH,
            path: settings_path,
        };

        this.load();
        debug!("persistence loaded: {:?}", this);

        this
    }

    fn get_file_time(&self) -> SystemTime {
        let metadata = fs::metadata(&self.path);

        match metadata {
            Ok(data) => {
                if let Ok(time) = data.modified() {
                    return time;
                }
            }
            Err(e) => {
                error!("{e}");
            }
        }

        panic!(
            "Cannot check file modification of '{}' on this platform",
            &self.path.display()
        );
    }

    fn load(&mut self) {
        let file = match File::open(&self.path) {
            Err(e) => {
                warn!(
                    "no config '{}' yet; starting fresh: {}",
                    &self.path.display(),
                    e
                );

                self.save();
                return;
            }
            Ok(f) => f,
        };

        let reader = BufReader::new(file);

        match serde_json::from_reader(reader) {
            Ok(u) => {
                self.config = u;
                info!("Loaded config from '{}'", &self.path.display());
            }
            Err(e) => {
                warn!(
                    "Config '{}' corrupted; starting fresh: {}",
                    &self.path.display(),
                    e
                );
            }
        };

        self.timestamp = self.get_file_time();
    }

    fn saver(&mut self) -> Result<(), Box<dyn Error>> {
        let file = File::create(&self.path)?;

        let mut writer = BufWriter::new(&file);

        serde_json::to_writer_pretty(writer.by_ref(), &self.config)?;
        write!(writer, "\n")?;
        writer.flush()?;

        info!("Written config file '{}'", &self.path.display());
        self.timestamp = self.get_file_time();
        Ok(())
    }

    fn save(&mut self) {
        match self.saver() {
            Err(e) => {
                warn!("cannot store config '{}': {}", &self.path.display(), e);
                return;
            }
            Ok(_) => {}
        };
    }

    pub fn store(&mut self, radar_info: &RadarInfo) {
        let mut modified = false;

        let radar = self
            .config
            .radars
            .entry(radar_info.key())
            .or_insert(Radar::default());

        let user_name = radar_info.controls.user_name();
        if radar.user_name != user_name {
            radar.user_name = user_name;
            modified = true;
        }
        let spoke_processing = radar_info.controls.spoke_processing();
        if radar.spoke_processing != spoke_processing {
            radar.spoke_processing = spoke_processing;
            modified = true;
        }
        let range_units = radar_info.controls.range_units();
        if radar.range_units != range_units {
            radar.range_units = range_units;
            modified = true;
        }
        let ranges = Some(radar_info.ranges.all.iter().map(|r| r.distance()).collect());
        if radar.ranges != ranges {
            radar.ranges = ranges;
            modified = true;
        }

        let model_name = radar_info.controls.model_name();
        if radar.model_name != model_name {
            radar.model_name = model_name;
            modified = true;
        }

        if modified {
            self.save();
        }
    }

    pub fn update_info_from_persistence(&self, info: &mut RadarInfo) {
        if let Some(p) = self.config.radars.get(&info.key()) {
            if p.model_name.is_some() {
                info.controls
                    .set_model_name(p.model_name.as_ref().unwrap().clone());
            }
            info.controls.set_user_name(p.user_name.clone());
            info.controls.set_spoke_processing(p.spoke_processing);
            info.controls.set_range_units(p.range_units);
            if let Some(ranges) = &p.ranges {
                if ranges.len() > 0 {
                    info.set_ranges(Ranges::new_by_distance(ranges));
                }
            }
        }
    }
}
