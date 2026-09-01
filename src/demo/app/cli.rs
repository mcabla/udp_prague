use crate::congestion::{fps_tp, time_tp, PRAGUE_INITMTU, PRAGUE_MAXRATE, PRAGUE_MINRATE};
use crate::core::{AppError, FRAME_DURATION, FRAME_PER_SECOND, PORT, RFC8888_ACKPERIOD};

use super::{AppStuff, REPT_PERIOD};

impl AppStuff {
    fn valid_filename(filename: &str) -> bool {
        if filename.is_empty() {
            return false;
        }
        if filename.as_bytes().first() == Some(&b'-') {
            return false;
        }

        const ILLEGAL: &[u8] = b"\\/:*?\"<>|";
        !filename.as_bytes().iter().any(|c| ILLEGAL.contains(c))
    }

    pub(super) fn parse_args(&mut self, args: &[String]) -> Result<(), AppError> {
        let mut index = 1usize;
        let default_addr = self.rcv_addr.clone();

        while index < args.len() {
            match args[index].as_str() {
                "-a" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("-a"))?;
                    self.rcv_addr = value.clone();
                }
                "-b" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("-b"))?;
                    let kbps: u64 = value
                        .parse()
                        .map_err(|_| Self::invalid_value("max bitrate"))?;
                    self.max_rate = kbps.saturating_mul(125);
                }
                "-p" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("-p"))?;
                    let port: u32 = value.parse().map_err(|_| Self::invalid_value("max port"))?;
                    if port > 65535 {
                        return Err(Self::invalid_value("max port"));
                    }
                    self.rcv_port = port;
                }
                "-c" => {
                    self.connect = true;
                }
                "--startuptimeout" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or(Self::missing_value("--startuptimeout"))?;
                    self.startup_wait_timeout_us = Some(
                        value
                            .parse()
                            .map_err(|_| Self::invalid_value("startup timeout"))?,
                    );
                }
                "-m" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("-m"))?;
                    self.max_pkt = value
                        .parse()
                        .map_err(|_| Self::invalid_value("max packet size"))?;
                }
                "-i" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("-i"))?;
                    let interval: u32 = value
                        .parse()
                        .map_err(|_| Self::invalid_value("min interval"))?;
                    if interval < 10_000 {
                        return Err(Self::invalid_value("min interval"));
                    }
                    self.rept_int = interval;
                    self.rept_tm = interval as time_tp;
                }
                "-v" => {
                    self.verbose = true;
                    self.quiet = true;
                }
                "-q" => {
                    self.quiet = true;
                }
                "-j" => {
                    index += 1;
                    let filename = args.get(index).ok_or(Self::missing_value("-j"))?;
                    if !Self::valid_filename(filename) {
                        return Err(Self::invalid_value("json filename"));
                    }
                    self.json_output = true;
                    self.jw.init(filename, false)?;
                }
                "--name" => {
                    index += 1;
                    let name = args.get(index).ok_or(Self::missing_value("--name"))?;
                    self.rept_name = name.clone();
                }
                "--rfc8888" => {
                    self.rfc8888_ack = true;
                }
                "--rfc8888ackperiod" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or(Self::missing_value("--rfc8888ackperiod"))?;
                    self.rfc8888_ackperiod = value
                        .parse()
                        .map_err(|_| Self::invalid_value("RFC8888 ACK period"))?;
                }
                "--rtmode" => {
                    self.rt_mode = true;
                }
                "--fps" => {
                    index += 1;
                    let value = args.get(index).ok_or(Self::missing_value("--fps"))?;
                    let fps: u32 = value
                        .parse()
                        .map_err(|_| Self::invalid_value("RT mode frame per second"))?;
                    self.rt_fps = fps as fps_tp;
                }
                "--frameduration" => {
                    index += 1;
                    let value = args
                        .get(index)
                        .ok_or(Self::missing_value("--frameduration"))?;
                    self.rt_frameduration = value
                        .parse()
                        .map_err(|_| Self::invalid_value("RT mode frame duration"))?;
                }
                "--no-classic-aqm-fallback" => {
                    self.classic_aqm_fallback_enabled = false;
                }
                _ => {
                    return Err(AppError::Usage(Self::usage(self.sender_role)));
                }
            }
            index += 1;
        }

        if self.connect && self.rcv_addr == default_addr {
            self.rcv_addr = "127.0.0.1".to_string();
        }
        if self.max_rate < PRAGUE_MINRATE || self.max_rate > PRAGUE_MAXRATE {
            self.max_rate = PRAGUE_MAXRATE;
        }
        if self.json_output && self.rept_name.is_empty() {
            self.rept_name = if self.sender_role {
                "sender"
            } else {
                "receiver"
            }
            .to_string();
        }
        if self.rt_mode {
            let product = (self.rt_fps as u64).saturating_mul(self.rt_frameduration as u64);
            if product > 1_000_000 {
                let fps = self.rt_fps.max(1) as u32;
                self.rt_frameduration = 1_000_000 / fps;
            }
        }

        Ok(())
    }

    /// Exit helper mirroring the reference `ExitIf`.
    ///
    /// The reference terminates the process; the Rust port returns an error so
    /// callers can decide how to surface failures.
    pub fn ExitIf(&self, cond: bool, msg: &str) -> Result<(), AppError> {
        if cond {
            return Err(AppError::Exit(msg.to_string()));
        }
        Ok(())
    }

    fn usage(sender: bool) -> String {
        format!(
            "UDP Prague {} usage:\n\
    -a <IP address, def: 0.0.0.0 or 127.0.0.1 if client>\n\
    -p <server port, def: {}>\n\
    -c (connect first as a client, otherwise bind and wait for connection)\n\
    --startuptimeout <optional trigger wait timeout in us for non-connected sender startup>\n\
    -b <sender specific max bitrate, def: {} kbps>\n\
    -m <max packet/ACK size, def: {} B>\n\
    -v (for verbose prints)\n\
    -i <report interval, def: {} us>\n\
    -j <json_filename, otherwise use stdout>\n\
    --name <json \"name\" value, def: {}>\n\
    -q (quiet)\n\
    --rfc8888 (RFC8888 feedback)\n\
    --rfc8888ackperiod <RFC8888 ACK period, def {} us>\n\
    --rtmode (Real-Time mode)\n\
    --fps <Frame-per-second, def {} fps>\n\
    --no-classic-aqm-fallback (disable the RFC 9331 Classic-AQM response)\n\
    --frameduration <Frame duration, def {} us>\n",
            if sender { "sender" } else { "receiver" },
            PORT,
            PRAGUE_MAXRATE / 125,
            PRAGUE_INITMTU,
            REPT_PERIOD,
            if sender { "sender" } else { "receiver" },
            RFC8888_ACKPERIOD,
            FRAME_PER_SECOND,
            FRAME_DURATION
        )
    }

    /// Print a short info line similar to the reference implementation.
    pub fn print_info(&self) {
        println!(
            "{} {} {} {} on port {} with max packet size {} bytes.",
            if !self.rt_mode {
                "UDP Prague"
            } else {
                "UDP RealTime Prague"
            },
            if self.sender_role {
                "sender"
            } else {
                "receiver"
            },
            if self.connect {
                "connecting to"
            } else {
                "listening at"
            },
            self.rcv_addr,
            self.rcv_port,
            self.max_pkt
        );
        if self.verbose {
            if self.sender_role {
                if !self.rt_mode {
                    println!(
                        "s: time, timestamp, echoed_timestamp, time_diff, seqnr, packet_size,,,,, pacing_rate, packet_window, packet_burst, packet_inflight, packet_inburst, nextSend"
                    );
                    println!(
                        "NORMAL_ACK_r: time, timestamp, echoed_timestamp, time_diff, seqnr, bytes_received, pkts_received, pkts_CE, pkts_lost, error_L4S,,,,, packet_inflight, packet_inburst, nextSend"
                    );
                    println!(
                        "RFC8888_ACK_r: time, begin_seq, num_reports, time_diff, seqnr, bytes_received, pkts_received, pkts_CE, pkts_lost, error_L4S,,,,, packet_inflight, packet_inburst, nextSend"
                    );
                } else {
                    println!(
                        "s: time, timestamp, echoed_timestamp, time_diff, seqnr, packet_size,,,,, pacing_rate, frame_window, frame_size, packet_burst, frame_inflight, frame_sent, packet_inburst, nextSend"
                    );
                    println!(
                        "NORMAL_ACK_r: time, timestamp, echoed_timestamp, time_diff, seqnr, bytes_received, pkts_received, pkts_CE, pkts_lost, error_L4S,,,,, frame_inflight, frame_sending, sent_frame, lost_frame, recv_frame, nextSend"
                    );
                    println!(
                        "RFC8888_ACK_r: time, begin_seq, num_reports, time_diff, seqnr, bytes_received, pkts_received, pkts_CE, pkts_lost, error_L4S,,,,, frame_inflight, frame_sending, sent_frame, lost_frame, recv_frame, nextSend"
                    );
                }
            } else {
                println!("r: time, timestamp, echoed_timestamp, time_diff, seqnr, bytes_received");
                println!(
                    "s: time, timestamp, echoed_timestamp, time_diff, seqnr, packet_size, pkts_received, pkts_CE, pkts_lost, error_L4S"
                );
            }
        }
    }
}
