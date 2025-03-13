use enum_primitive_derive::Primitive;
use std::fmt::Write;
use tokio::io::{AsyncWriteExt, WriteHalf};
use tokio::net::TcpStream;

use super::CommandMode;
use crate::brand::furuno::FURUNO_RADAR_RANGES;
use crate::radar::{RadarError, RadarInfo};
use crate::settings::{ControlType, ControlValue, SharedControls};

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
    AntennaRevolution = 0x89,
    AntennaSwitch = 0x8A,
    AntennaNo = 0x8D,
    OnTime = 0x8E,

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

pub struct Command {
    key: String,
    controls: SharedControls,
}

impl Command {
    pub fn new(info: &RadarInfo) -> Self {
        Command {
            key: info.key(),
            controls: info.controls.clone(),
        }
    }

    pub async fn send(
        &mut self,
        write: &mut WriteHalf<TcpStream>,
        cm: CommandMode,
        id: CommandId,
        args: &[i32],
    ) -> Result<(), RadarError> {
        let mut message = format!("${}{:X}", cm.to_char(), id as u32);
        for arg in args {
            let _ = write!(&mut message, ",{}", arg);
        }

        log::trace!("{}: sending {}", self.key, message);

        message.push('\r');
        message.push('\n');

        let bytes = message.into_bytes();

        write.write_all(&bytes).await.map_err(RadarError::Io)?;

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
        let mut cmd = Vec::with_capacity(6);

        cmd.push(
            sector1_start.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitStart1)),
        );
        cmd.push(sector1_end.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitEnd1)));
        cmd.push(
            sector2_start.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitStart2)),
        );
        cmd.push(sector2_end.unwrap_or_else(|| self.get_angle_value(&ControlType::NoTransmitEnd2)));

        cmd
    }

    pub async fn set_control(
        &mut self,
        write: &mut WriteHalf<TcpStream>,
        cv: &ControlValue,
    ) -> Result<(), RadarError> {
        let value = cv
            .value
            .parse::<f32>()
            .map_err(|_| RadarError::MissingValue(cv.id))? as i32;
        let auto: i32 = if cv.auto.unwrap_or(false) { 1 } else { 0 };
        let enabled: i32 = if cv.enabled.unwrap_or(false) { 1 } else { 0 };

        let mut cmd = Vec::with_capacity(6);

        let id: CommandId = match cv.id {
            ControlType::Status => {
                cmd.push(value); // status
                cmd.push(0);
                cmd.push(0); // WatchMan on/off
                cmd.push(60); // Watchman On time?
                cmd.push(300); // Watchman Off time?
                cmd.push(0); // Always 0

                CommandId::Status
            }

            ControlType::Range => {
                cmd.push(if value < FURUNO_RADAR_RANGES.len() as i32 {
                    value
                } else {
                    let mut i = 0;
                    for r in FURUNO_RADAR_RANGES {
                        if r >= value {
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
                cmd.push(0);
                cmd.push(value);
                cmd.push(auto);
                cmd.push(value);
                cmd.push(0);
                CommandId::Gain
            }
            ControlType::Sea => {
                cmd.push(value);
                CommandId::Sea
            }
            ControlType::Rain => {
                cmd.push(value);
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
            ControlType::ScanSpeed => CommandId::AntennaRevolution,
            ControlType::AntennaHeight => CommandId::AntennaHeight,

            // Non-hardware settings
            _ => return Err(RadarError::CannotSetControlType(cv.id)),
        };

        log::info!(
            "{}: Send command {:02X},{:?}",
            self.key,
            id.clone() as u32,
            cmd
        );
        self.send(write, CommandMode::Set, id, &cmd).await?;
        self.send(
            write,
            CommandMode::Request,
            CommandId::CustomPictureAll,
            &[],
        )
        .await?; // $R66
        Ok(())
    }

    pub(crate) async fn init(
        &mut self,
        write: &mut WriteHalf<TcpStream>,
    ) -> Result<(), RadarError> {
        self.send(write, CommandMode::Request, CommandId::DispMode, &[0, 0, 0])
            .await?; // $R61,0,0,0
        self.send(write, CommandMode::Request, CommandId::Range, &[0, 0, 0])
            .await?; // $R62,0,0,0

        // Can probably be derived from CustomPictureAll
        self.send(
            write,
            CommandMode::Request,
            CommandId::Gain,
            &[0, 0, 0, 0, 0],
        )
        .await?; // $R63,0,0,0,0,0

        self.send(
            write,
            CommandMode::Request,
            CommandId::CustomPictureAll,
            &[],
        )
        .await?; // $R66
        self.send(
            write,
            CommandMode::Request,
            CommandId::Status,
            &[0, 0, 0, 0, 0, 0],
        )
        .await?; // $R66,0,0,0,0,0,0

        self.send(
            write,
            CommandMode::Request,
            CommandId::AntennaType,
            &[0, 0, 0, 0, 0, 0],
        )
        .await?; // $R6E,0,0,0,0,0,0,0

        self.send(
            write,
            CommandMode::Request,
            CommandId::BlindSector,
            &[0, 0, 0, 0, 0],
        )
        .await?; // $R77,0,0,0,0,0

        self.send(
            write,
            CommandMode::Request,
            CommandId::MainBangSize,
            &[0, 0],
        )
        .await?; // $R83,0,0

        self.send(
            write,
            CommandMode::Request,
            CommandId::AntennaHeight,
            &[0, 0],
        )
        .await?; // $R84,0,0

        self.send(write, CommandMode::Request, CommandId::NearSTC, &[0])
            .await?; // $R85,0

        self.send(write, CommandMode::Request, CommandId::MiddleSTC, &[0])
            .await?; // $R86,0

        self.send(write, CommandMode::Request, CommandId::FarSTC, &[0])
            .await?; // $R87,0

        self.send(write, CommandMode::Request, CommandId::OnTime, &[0, 0])
            .await?; // $R8E,0

        self.send(write, CommandMode::Request, CommandId::WakeUpCount, &[0])
            .await?; // $RAC,0

        Ok(())
    }

    pub(super) async fn send_report_requests(&mut self) -> Result<(), RadarError> {
        Ok(())
    }
}
