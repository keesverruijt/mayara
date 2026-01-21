use async_trait::async_trait;
use enum_primitive_derive::Primitive;
use std::fmt::Write;
use std::str::FromStr;
use tokio::io::{AsyncWriteExt, WriteHalf};
use tokio::net::TcpStream;

use super::CommandMode;
use crate::radar::range::Ranges;
use crate::radar::{CommandSender, RadarError, RadarInfo, Status};
use crate::settings::{ControlType, ControlValue, SharedControls};

const RADAR_A: i32 = 0;

#[derive(Primitive, PartialEq, Eq, Debug, Clone)]
pub(crate) enum CommandId {
    Connect = 0x60,
    DispMode = 0x61,
    Range = 0x62,
    Gain = 0x63,
    Sea = 0x64,
    Rain = 0x65,
    CustomPictureAll = 0x66,
    SignalProcessing = 0x67,  // Multi-purpose: NoiseReduction, InterferenceRejection, etc.
    Status = 0x69,
    U6D = 0x6D,
    AntennaType = 0x6E,

    BlindSector = 0x77,

    Att = 0x80,
    MainBangSize = 0x83,
    AntennaHeight = 0x84,
    NearSTC = 0x85,
    MiddleSTC = 0x86,
    FarSTC = 0x87,
    ScanSpeed = 0x89,
    AntennaSwitch = 0x8A,
    AntennaNo = 0x8D,
    OnTime = 0x8E,
    TxTime = 0x8F,

    Modules = 0x96,

    Drift = 0x9E,
    ConningPosition = 0xAA,
    WakeUpCount = 0xAC,

    STCRange = 0xD2,
    CustomMemory = 0xD3,
    BuildUpTime = 0xD4,
    DisplayUnitInformation = 0xD5,
    CustomATFSettings = 0xE0,
    AliveCheck = 0xE3,
    ATFSettings = 0xEA,
    BirdMode = 0xED,
    RezBoost = 0xEE,        // Target Separation (beam sharpening)
    TargetAnalyzer = 0xEF,  // Doppler mode
    AutoAcquire = 0xF0,
    RangeSelect = 0xFE,
}

/// Furuno wire index to meters mapping table
/// CRITICAL: Wire indices are NON-SEQUENTIAL! The radar uses specific wire index values.
/// Verified via Wireshark captures from TimeZero ↔ DRS4D-NXT
///
/// Example: To set 1/16nm range, you send wire_index=21 (NOT 0!)
///          To set 36nm range, you send wire_index=19 (NOT 16!)
pub const WIRE_INDEX_TABLE: [(i32, i32); 18] = [
    (21, 116),   // 1/16 nm = 116m (minimum range) - wire index 21!
    (0, 231),    // 1/8 nm = 231m
    (1, 463),    // 1/4 nm = 463m
    (2, 926),    // 1/2 nm = 926m
    (3, 1389),   // 3/4 nm = 1389m
    (4, 1852),   // 1 nm = 1852m
    (5, 2778),   // 1.5 nm = 2778m
    (6, 3704),   // 2 nm = 3704m
    (7, 5556),   // 3 nm = 5556m
    (8, 7408),   // 4 nm = 7408m
    (9, 11112),  // 6 nm = 11112m
    (10, 14816), // 8 nm = 14816m
    (11, 22224), // 12 nm = 22224m
    (12, 29632), // 16 nm = 29632m
    (13, 44448), // 24 nm = 44448m
    (14, 59264), // 32 nm = 59264m
    (19, 66672), // 36 nm = 66672m (OUT OF SEQUENCE! wire index 19!)
    (15, 88896), // 48 nm = 88896m (maximum range)
];

/// Convert meters to Furuno wire index
/// Uses exact match lookup in the WIRE_INDEX_TABLE.
pub fn meters_to_wire_index(meters: i32) -> i32 {
    // Try exact match first
    for (wire_idx, m) in WIRE_INDEX_TABLE.iter() {
        if *m == meters {
            return *wire_idx;
        }
    }
    // If no exact match, find the closest one that's >= requested meters
    for (wire_idx, m) in WIRE_INDEX_TABLE.iter() {
        if *m >= meters {
            return *wire_idx;
        }
    }
    // Fallback to max range (48nm = wire index 15)
    15
}

/// Convert Furuno wire index to meters
pub fn wire_index_to_meters(wire_index: i32) -> Option<i32> {
    WIRE_INDEX_TABLE
        .iter()
        .find(|(idx, _)| *idx == wire_index)
        .map(|(_, meters)| *meters)
}

pub(crate) struct Command {
    key: String,
    write: Option<WriteHalf<TcpStream>>,
    controls: SharedControls,
    ranges: Ranges,
}

impl Command {
    pub fn new(info: &RadarInfo) -> Self {
        Command {
            key: info.key(),
            write: None,
            controls: info.controls.clone(),
            ranges: info.ranges.clone(),
        }
    }

    pub fn set_writer(&mut self, write: WriteHalf<TcpStream>) {
        self.write = Some(write);
    }

    pub fn set_ranges(&mut self, ranges: Ranges) {
        self.ranges = ranges;
    }

    pub async fn send(
        &mut self,
        cm: CommandMode,
        id: CommandId,
        args: &[i32],
    ) -> Result<(), RadarError> {
        self.send_with_commas(cm, id, args, 0).await
    }

    pub async fn send_with_commas(
        &mut self,
        cm: CommandMode,
        id: CommandId,
        args: &[i32],
        commas: u32,
    ) -> Result<(), RadarError> {
        let mut message = format!("${}{:X}", cm.to_char(), id as u32);
        for arg in args {
            let _ = write!(&mut message, ",{}", arg);
        }
        for _ in 0..commas {
            message.push(',');
        }

        log::trace!("{}: sending {}", self.key, message);

        if commas == 0 {
            message.push('\r');
        }
        message.push('\n');

        let bytes = message.into_bytes();

        match &mut self.write {
            Some(w) => {
                w.write_all(&bytes).await.map_err(RadarError::Io)?;
            }
            None => return Err(RadarError::NotConnected),
        };

        Ok(())
    }

    fn get_angle_value(&self, ct: &ControlType) -> i32 {
        if let Some(control) = self.controls.get(ct) {
            if let Some(value) = control.value {
                return value as i32;
            }
        }
        return 0;
    }

    fn fill_blind_sector(
        &mut self,
        sector1_start: Option<i32>,
        sector1_end: Option<i32>,
        sector2_start: Option<i32>,
        sector2_end: Option<i32>,
    ) -> Vec<i32> {
        let mut cmd = Vec::with_capacity(5);

        // Get current values
        let s1_start = sector1_start.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitStart1));
        let s1_end = sector1_end.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitEnd1));
        let s2_start = sector2_start.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitStart2));
        let s2_end = sector2_end.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitEnd2));

        // Calculate widths from start/end angles
        let s1_width = if s1_end >= s1_start {
            s1_end - s1_start
        } else {
            360 + s1_end - s1_start
        };

        let s2_width = if s2_end >= s2_start {
            s2_end - s2_start
        } else {
            360 + s2_end - s2_start
        };

        // Format: $S77,{s2_enable},{s1_start},{s1_width},{s2_start},{s2_width}
        let s2_enable = if s2_width > 0 { 1 } else { 0 };
        cmd.push(s2_enable);
        cmd.push(s1_start);
        cmd.push(s1_width);
        cmd.push(s2_start);
        cmd.push(s2_width);

        cmd
    }

    pub(crate) async fn init(&mut self) -> Result<(), RadarError> {
        // Query firmware/model information
        self.send(CommandMode::Request, CommandId::Modules, &[])
            .await?; // $R96

        // Query operating hours
        self.send(CommandMode::Request, CommandId::OnTime, &[0])
            .await?; // $R8E,0

        // Query transmit hours
        self.send(CommandMode::Request, CommandId::TxTime, &[0])
            .await?; // $R8F,0

        // Query current state of all controls
        self.send(CommandMode::Request, CommandId::Status, &[])
            .await?; // $R69

        self.send(CommandMode::Request, CommandId::Range, &[])
            .await?; // $R62

        self.send(CommandMode::Request, CommandId::Gain, &[])
            .await?; // $R63

        self.send(CommandMode::Request, CommandId::Sea, &[])
            .await?; // $R64

        self.send(CommandMode::Request, CommandId::Rain, &[])
            .await?; // $R65

        self.send(CommandMode::Request, CommandId::ScanSpeed, &[])
            .await?; // $R89

        self.send(CommandMode::Request, CommandId::MainBangSize, &[0, 0])
            .await?; // $R83,0,0

        self.send(CommandMode::Request, CommandId::BlindSector, &[])
            .await?; // $R77

        // NXT-specific features (query signal processing features)
        self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 3])
            .await?; // $R67,0,3 - Noise Reduction

        self.send(CommandMode::Request, CommandId::SignalProcessing, &[0, 0])
            .await?; // $R67,0,0 - Interference Rejection

        self.send(CommandMode::Request, CommandId::RezBoost, &[])
            .await?; // $REE - Beam sharpening (Target Separation)

        self.send(CommandMode::Request, CommandId::BirdMode, &[])
            .await?; // $RED - Bird mode

        self.send(CommandMode::Request, CommandId::TargetAnalyzer, &[])
            .await?; // $REF - Target Analyzer (Doppler)

        Ok(())
    }

    pub async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        log::debug!("{}: send_report_requests", self.key);

        self.send(CommandMode::Request, CommandId::AliveCheck, &[])
            .await?;
        Ok(())
    }
}

#[async_trait]
impl CommandSender for Command {
    async fn set_control(
        &mut self,
        cv: &ControlValue,
        _: &SharedControls,
    ) -> Result<(), RadarError> {
        let value = cv
            .value
            .parse::<f32>()
            .map_err(|_| RadarError::MissingValue(cv.id))? as i32;
        let auto: i32 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let _enabled: i32 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        log::trace!("set_control: {:?} = {} => {:.1}", cv.id, cv.value, value);

        let mut cmd = Vec::with_capacity(6);

        let id: CommandId = match cv.id {
            ControlType::Status => {
                let value = match Status::from_str(&cv.value).unwrap_or(Status::Standby) {
                    Status::Transmit => 2,
                    _ => 1,
                };

                cmd.push(value); // status
                cmd.push(0);
                cmd.push(0); // WatchMan on/off
                cmd.push(60); // Watchman On time?
                cmd.push(300); // Watchman Off time?
                cmd.push(0); // Always 0

                CommandId::Status
            }

            ControlType::Range => {
                // CRITICAL: Must use wire index, not array position!
                // Wire indices are non-sequential (21=min, 0-15=normal, 19=36nm out of order)
                let wire_index = meters_to_wire_index(value);
                cmd.push(wire_index);
                cmd.push(0);
                cmd.push(0);
                CommandId::Range
            }

            ControlType::Gain => {
                // Format: $S63,{auto},{value},0,80,0
                // From pcap: $S63,0,50,0,80,0 (manual, value=50)
                cmd.push(auto);
                cmd.push(value);
                cmd.push(0);
                cmd.push(80);
                cmd.push(0);
                CommandId::Gain
            }
            ControlType::Sea => {
                // Format: $S64,{auto},{value},50,0,0,0
                // From pcap: $S64,{auto},{value},50,0,0,0
                cmd.push(auto);
                cmd.push(value);
                cmd.push(50);
                cmd.push(0);
                cmd.push(0);
                cmd.push(0);
                CommandId::Sea
            }
            ControlType::Rain => {
                // Format: $S65,{auto},{value},0,0,0,0
                // From pcap: $S65,{auto},{value},0,0,0,0
                cmd.push(auto);
                cmd.push(value);
                cmd.push(0);
                cmd.push(0);
                cmd.push(0);
                cmd.push(0);
                CommandId::Rain
            }

            ControlType::NoTransmitStart1 => {
                cmd = self.fill_blind_sector(Some(value), None, None, None);

                CommandId::BlindSector
            }
            ControlType::NoTransmitEnd1 => {
                cmd = self.fill_blind_sector(None, Some(value), None, None);

                CommandId::BlindSector
            }
            ControlType::NoTransmitStart2 => {
                cmd = self.fill_blind_sector(None, None, Some(value), None);

                CommandId::BlindSector
            }
            ControlType::NoTransmitEnd2 => {
                cmd = self.fill_blind_sector(None, None, None, Some(value));

                CommandId::BlindSector
            }
            ControlType::ScanSpeed => {
                // Format: $S89,{mode},0 where mode: 0=24RPM, 2=Auto
                cmd.push(value);
                cmd.push(0);
                CommandId::ScanSpeed
            }
            ControlType::AntennaHeight => {
                // Format: $S84,0,{meters},0
                cmd.push(0);
                cmd.push(value);
                cmd.push(0);
                CommandId::AntennaHeight
            }
            ControlType::MainBangSuppression => {
                // Format: $S83,{value_255},0
                // Map 0-100% to 0-255
                let value_255 = (value * 255) / 100;
                cmd.push(value_255);
                cmd.push(0);
                CommandId::MainBangSize
            }

            // NXT-specific features
            ControlType::NoiseRejection => {
                // Format: $S67,0,3,{enabled},0
                // Feature 3 = Noise Reduction
                let enabled = if value > 0 { 1 } else { 0 };
                cmd.push(0);
                cmd.push(3);
                cmd.push(enabled);
                cmd.push(0);
                CommandId::SignalProcessing
            }
            ControlType::InterferenceRejection => {
                // Format: $S67,0,0,{enabled},0
                // Feature 0 = Interference Rejection
                // Note: enabled=2 (not 1) per protocol spec
                let enabled = if value > 0 { 2 } else { 0 };
                cmd.push(0);
                cmd.push(0);
                cmd.push(enabled);
                cmd.push(0);
                CommandId::SignalProcessing
            }
            ControlType::TargetSeparation => {
                // Format: $SEE,{level},0
                // RezBoost (beam sharpening): 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(0); // screen: 0=Primary
                CommandId::RezBoost
            }
            ControlType::BirdMode => {
                // Format: $SED,{level},0
                // BirdMode: 0=OFF, 1=Low, 2=Medium, 3=High
                cmd.push(value);
                cmd.push(0); // screen: 0=Primary
                CommandId::BirdMode
            }
            ControlType::Doppler => {
                // Format: $SEF,{enabled},{mode},0
                // Target Analyzer: value 0=Off, 1=Target, 2=Rain
                // Wire format: enabled=0/1, mode=0(Target)/1(Rain)
                let (enabled, mode) = match value {
                    0 => (0, 0), // Off
                    1 => (1, 0), // Target
                    2 => (1, 1), // Rain
                    _ => (0, 0), // Invalid, default to Off
                };
                cmd.push(enabled);
                cmd.push(mode);
                cmd.push(0); // screen: 0=Primary
                CommandId::TargetAnalyzer
            }

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlType(cv.id)),
        };

        log::info!(
            "{}: Send command {:02X},{:?}",
            self.key,
            id.clone() as u32,
            cmd
        );

        self.send(CommandMode::Set, id, &cmd).await?;
        self.send(CommandMode::Request, CommandId::CustomPictureAll, &[])
            .await?; // $R66
        Ok(())
    }
}
