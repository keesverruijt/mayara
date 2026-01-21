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
    CustomPicture = 0x67,
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
    BearingResolutionSetting = 0xEE,
    AccuShip = 0xF0,
    RangeSelect = 0xFE,
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
        self.send(CommandMode::Request, CommandId::Connect, &[0])
            .await?; // $R60,0,0,0,0,0,0,0, Furuno sends with just separated commas.

        self.send_with_commas(CommandMode::Request, CommandId::Modules, &[], 7)
            .await?; // $R96,,,,,,,

        self.send(CommandMode::Request, CommandId::Range, &[0, 0, 0])
            .await?; // $R62,0,0,0

        self.send(CommandMode::Request, CommandId::CustomPictureAll, &[])
            .await?; // $R66
        self.send(CommandMode::Request, CommandId::Status, &[0, 0, 0, 0, 0, 0])
            .await?; // $R66,0,0,0,0,0,0

        self.send(
            CommandMode::Request,
            CommandId::AntennaType,
            &[0, 0, 0, 0, 0, 0],
        )
        .await?; // $R6E,0,0,0,0,0,0,0

        self.send(
            CommandMode::Request,
            CommandId::BlindSector,
            &[0, 0, 0, 0, 0],
        )
        .await?; // $R77,0,0,0,0,0

        self.send(CommandMode::Request, CommandId::MainBangSize, &[0, 0])
            .await?; // $R83,0,0

        self.send(CommandMode::Request, CommandId::AntennaHeight, &[0, 0])
            .await?; // $R84,0,0

        self.send(CommandMode::Request, CommandId::NearSTC, &[0])
            .await?; // $R85,0

        self.send(CommandMode::Request, CommandId::MiddleSTC, &[0])
            .await?; // $R86,0

        self.send(CommandMode::Request, CommandId::FarSTC, &[0])
            .await?; // $R87,0

        self.send(CommandMode::Request, CommandId::OnTime, &[0, 0])
            .await?; // $R8E,0

        self.send(CommandMode::Request, CommandId::WakeUpCount, &[0])
            .await?; // $RAC,0

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
                let ranges = &self.ranges;
                cmd.push(if value < ranges.len() as i32 {
                    value
                } else {
                    let mut i = 0;
                    for r in ranges.all.iter() {
                        if r.distance() >= value {
                            break;
                        }
                        i += 1;
                    }
                    i
                });
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
