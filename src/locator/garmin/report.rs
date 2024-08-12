use log::{debug, trace, warn};

#[repr(C)]
struct rad_ctl_pkt_9 {
    parm1: u8,
}

#[repr(C)]
struct rad_ctl_pkt_10 {
    parm1: u16,
}

#[repr(C)]
struct rad_ctl_pkt_12 {
    parm1: u32,
}

#[repr(C)]
struct rad_respond_pkt_16 {
    parm1: u32,
    parm2: u32,
    parm3: u16,
    parm4: u16,
    parm5: u8,
    parm6: u8,
    parm7: u16,
}

#[repr(C)]
struct rad_pkt_0x099b {
    parm1: u16,
    parm2: u16,
    parm3: u32,
    parm4: u32,
    parm5: u32,
    info: [u8; 64],
}

pub fn process(report: &[u8]) {
    if report.len() < size_of::<u32>() * 2 + size_of::<rad_ctl_pkt_9>() {
        return;
    }

    trace!("Garmin report: {:?}", report);

    let (packet_type, data) = report.split_at(std::mem::size_of::<u32>());
    let (len, data) = data.split_at(std::mem::size_of::<u32>());
    let packet_type = u32::from_le_bytes(packet_type.try_into().unwrap());
    let len = u32::from_le_bytes(len.try_into().unwrap());

    debug!("Garmin message type {} len {}", packet_type, len);

    if data.len() != len as usize {
        warn!(
            "Received incomplete message of len {} expected {}",
            data.len(),
            len
        );
        return;
    }
    let value: u32 = match len {
        1 => data[0] as u32,
        2 => {
            let ins_bytes = <[u8; 2]>::try_from(&data[0..2]).unwrap();
            u16::from_be_bytes(ins_bytes) as u32
        }
        4 => {
            let ins_bytes = <[u8; 4]>::try_from(&data[0..4]).unwrap();
            u32::from_be_bytes(ins_bytes)
        }
        _ => 0,
    };

    match packet_type {
        0x0916 => {
            trace!("Scan speed {}", value);
        }
        0x0919 => {
            trace!("Transmit state {}", value);
        }
        0x091e => {
            trace!("Range {} m", value);
        }
        0x0919 => {
            trace!("Transmit state {}", value);
        }
        //
        // Garmin sends range in three separate packets, in the order 0x924, 0x925, 0x91d every
        // two seconds.
        // Auto High: 0x924 = 2, 0x925 = gain, 0x91d = 1
        // Auto Low:  0x924 = 2, 0x925 = gain, 0x91d = 0
        // Manual:    0x924 = 0, 0x925 = gain, 0x91d = 0 (could be last one used?)
        0x0924 => {
            trace!("Autogain {}", value);
        }
        0x0925 => {
            trace!("Gain {}", value);
        }
        0x091d => {
            trace!("Autogain level {}", value);
        }
        0x0930 => {
            trace!("Bearing alignment {}", value as f32 / 32.0);
        }
        0x0932 => {
            trace!("Crosstalk rejection {}", value);
        }
        0x0933 => {
            trace!("Rain clutter mode {}", value);
        }
        0x0934 => {
            trace!("Rain clutter level {}", value);
        }
        0x0939 => {
            trace!("Sea clutter mode {}", value);
        }
        0x093a => {
            trace!("Sea clutter level {}", value);
        }
        0x093b => {
            trace!("Sea clutter auto level {}", value);
        }
        _ => {}
    }
    /*

          case 0x093f: {
            LOG_VERBOSE(wxT("Garmin xHD 0x093a: no transmit zone mode %d"), packet9->parm1);
            m_no_transmit_zone_mode = packet9->parm1 > 0;
            // parm1 = 0 = Zone off, in that case we want AUTO_RANGE - 1 = 'Off'.
            // parm1 = 1 = Zone on, in that case we will receive 0x0940+0x0941.
            if (!m_no_transmit_zone_mode) {
              m_ri->m_no_transmit_start[0].Update(0, RCS_OFF);
              m_ri->m_no_transmit_end[0].Update(0, RCS_OFF);
            }
            m_ri->m_no_transmit_zones = 1;
            return true;
          }
          case 0x0940: {
            LOG_VERBOSE(wxT("Garmin xHD 0x0940: no transmit zone start %d"), (int32_t)packet12->parm1 / 32);
            if (m_no_transmit_zone_mode) {
              m_ri->m_no_transmit_start[0].Update((int32_t)packet12->parm1 / 32, RCS_MANUAL);
            }
            return true;
          }
          case 0x0941: {
            LOG_VERBOSE(wxT("Garmin xHD 0x0941: no transmit zone end %d"), (int32_t)packet12->parm1 / 32);
            if (m_no_transmit_zone_mode) {
              m_ri->m_no_transmit_end[0].Update((int32_t)packet12->parm1 / 32, RCS_MANUAL);
            }
            return true;
          }
          case 0x02bb: {
            LOG_VERBOSE(wxT("Garmin xHD 0x02bb: something %d"), (int32_t)packet12->parm1);
            return true;
          }
          case 0x02ec: {
            LOG_VERBOSE(wxT("Garmin xHD 0x02ec: something %d"), (int32_t)packet12->parm1);
            return true;
          }
          case 0x0942: {
            LOG_VERBOSE(wxT("Garmin xHD 0x0942: timed idle mode %d"), (int32_t)packet9->parm1);
            if (packet9->parm1 == 0) {
              m_timed_idle_mode = RCS_OFF;
            } else {
              m_timed_idle_mode = RCS_MANUAL;
            }
            return true;
          }

          case 0x0943: {
            LOG_VERBOSE(wxT("Garmin xHD 0x0943: timed idle time %d s"), (int32_t)packet10->parm1);
            m_ri->m_timed_idle.Update(packet10->parm1 / 60, m_timed_idle_mode);

            return true;
          }

          case 0x0944: {
            LOG_VERBOSE(wxT("Garmin xHD 0x0944: timed run time %d s"), (int32_t)packet10->parm1);
            m_ri->m_timed_run.Update(packet10->parm1 / 60);
            return true;
          }

          case 0x0992: {
            // Scanner state
            if (UpdateScannerStatus(packet9->parm1)) {
              return true;
            }
          }

          case 0x0993: {
            // State change announce
            LOG_VERBOSE(wxT("Garmin xHD 0x0993: state-change in %d ms"), packet12->parm1);
            m_ri->m_next_state_change.Update(packet12->parm1 / 1000);
            return true;
          }

          case 0x099b: {
            rad_pkt_0x099b *packet = (rad_pkt_0x099b *)report;

            // Not sure that this always contains an error message
            // Observed with Timed Transmit (hardware control via plotter) it reports
            // 'State machine event fault - unhandled state transition request'

            LOG_INFO(wxT("Garmin xHD 0x099b: error '%s'"), packet->info);
            return true;
          }

        }

    if report[0] == 0x01 && report[1] == 0xB1 {
        // Wake radar
        debug!("Wake radar request from {}", from);
        return;
    }
    if report[0] == 0x1 && report[1] == 0xB2 {
        // Common Navico message from 4G++
        if report.len() < size_of::<NavicoBeaconSingle>() {
            debug!("Incomplete beacon from {}, length {}", from, report.len());
            return;
        }

        if report.len() >= NAVICO_BEACON_DUAL_SIZE {
            match deserialize::<NavicoBeaconDual>(report) {
                Ok(data) => {
                    if let Some(serial_no) = c_string(&data.header.serial_no) {
                        let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                        let radar_data: SocketAddrV4 = data.a.data.into();
                        let radar_report: SocketAddrV4 = data.a.report.into();
                        let radar_send: SocketAddrV4 = data.a.send.into();
                        let location_info: RadarLocationInfo = RadarLocationInfo::new(
                            serial_no,
                            Some("A"),
                            radar_addr,
                            radar_data,
                            radar_report,
                            radar_send,
                        );
                        debug!("Received beacon from {}", &location_info);

                        let radar_data: SocketAddrV4 = data.b.data.into();
                        let radar_report: SocketAddrV4 = data.b.report.into();
                        let radar_send: SocketAddrV4 = data.b.send.into();
                        let location_info: RadarLocationInfo = RadarLocationInfo::new(
                            serial_no,
                            Some("B"),
                            radar_addr,
                            radar_data,
                            radar_report,
                            radar_send,
                        );
                        debug!("Received beacon from {}", &location_info);
                    }
                }
                Err(e) => {
                    error!("Failed to decode dual range data: {}", e);
                }
            }
            return;
        }

        if report.len() >= NAVICO_BEACON_SINGLE_SIZE {
            match deserialize::<NavicoBeaconSingle>(report) {
                Ok(data) => {
                    if let Some(serial_no) = c_string(&data.header.serial_no) {
                        let radar_addr: SocketAddrV4 = data.header.radar_addr.into();

                        let radar_data: SocketAddrV4 = data.a.data.into();
                        let radar_report: SocketAddrV4 = data.a.report.into();
                        let radar_send: SocketAddrV4 = data.a.send.into();
                        let location_info: RadarLocationInfo = RadarLocationInfo::new(
                            serial_no,
                            None,
                            radar_addr,
                            radar_data,
                            radar_report,
                            radar_send,
                        );
                        debug!("Received beacon from {}", &location_info);
                    }
                }
                Err(e) => {
                    error!("Failed to decode dual range data: {}", e);
                }
            }
            return;
        }
    }

        */
}
