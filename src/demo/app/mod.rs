//! Application-level utilities and CLI configuration.
//!
//! The C++ reference project centralizes argument parsing and logging helpers in
//! `app_stuff.h`. This Rust port keeps the same data layout and method names
//! while splitting the implementation into smaller internal modules.
//!
//! This module is part of the demo application's compatibility layer and is
//! available only when the `demo-app` feature is enabled.

mod cli;
mod glue;
mod reporting;

#[cfg(test)]
mod tests;

use super::json_writer::json_writer;
use crate::congestion::{
    count_tp, fps_tp, rate_tp, size_tp, time_tp, PRAGUE_INITMTU, PRAGUE_MAXRATE,
};
use crate::core::{
    AppError, RunnerConfig, FRAME_DURATION, FRAME_PER_SECOND, PORT, RFC8888_ACKPERIOD,
};

/// Default reporting period (µs).
pub const REPT_PERIOD: u32 = 1_000_000;

/// App state (arguments + reporting accumulators) used by the example binaries.
///
/// This is intentionally close to the reference struct to ease side-by-side
/// comparisons and allow near-literal porting of the sender/receiver loops.
#[derive(Debug)]
pub struct AppStuff {
    /// Whether this binary is the sender role.
    pub sender_role: bool,
    /// Verbose output.
    pub verbose: bool,
    /// Quiet output.
    pub quiet: bool,
    /// Receiver address.
    pub rcv_addr: String,
    /// Receiver port.
    pub rcv_port: u32,
    /// Whether to `connect()` rather than `bind()`.
    pub connect: bool,
    /// Optional timeout for the non-connected sender startup trigger wait (µs).
    pub startup_wait_timeout_us: Option<u32>,
    /// Whether to output JSON.
    pub json_output: bool,
    /// Whether JSON output has already failed and been reported.
    pub json_output_failed: bool,
    /// JSON writer (initialized only if `json_output=true`).
    pub jw: json_writer,
    /// Max packet size.
    pub max_pkt: size_tp,
    /// Max pacing rate.
    pub max_rate: rate_tp,
    /// Send diff reference timestamp.
    pub data_tm: time_tp,
    /// ACK diff reference timestamp.
    pub ack_tm: time_tp,
    /// Timer for reporting interval.
    pub rept_tm: time_tp,
    /// Report interval (µs).
    pub rept_int: u32,
    /// Optional name (used as JSON `name`).
    pub rept_name: String,
    /// Accumulated bytes sent.
    pub acc_bytes_sent: rate_tp,
    /// Accumulated bytes received.
    pub acc_bytes_rcvd: rate_tp,
    /// Accumulated RTTs (for average).
    pub acc_rtts: rate_tp,
    /// Count RTT reports.
    pub count_rtts: count_tp,
    /// Previous packets received.
    pub prev_pkts: count_tp,
    /// Previous marks received.
    pub prev_marks: count_tp,
    /// Previous losts received.
    pub prev_losts: count_tp,
    /// RFC8888 ACK mode.
    pub rfc8888_ack: bool,
    /// RFC8888 ACK period (µs).
    pub rfc8888_ackperiod: u32,
    /// Real-time (frame) mode.
    pub rt_mode: bool,
    /// Real-time FPS.
    pub rt_fps: fps_tp,
    /// Real-time frame duration (µs).
    pub rt_frameduration: u32,
}

impl AppStuff {
    /// Construct from CLI args.
    ///
    /// `sender=true` selects sender defaults, otherwise receiver defaults.
    pub fn new(sender: bool, args: &[String]) -> Result<Self, AppError> {
        let mut app = Self {
            sender_role: sender,
            verbose: false,
            quiet: false,
            rcv_addr: "0.0.0.0".to_string(),
            rcv_port: PORT as u32,
            connect: false,
            startup_wait_timeout_us: None,
            json_output: false,
            json_output_failed: false,
            jw: json_writer::new(),
            max_pkt: PRAGUE_INITMTU,
            max_rate: PRAGUE_MAXRATE,
            data_tm: 1,
            ack_tm: 1,
            rept_tm: REPT_PERIOD as time_tp,
            rept_int: REPT_PERIOD,
            rept_name: String::new(),
            acc_bytes_sent: 0,
            acc_bytes_rcvd: 0,
            acc_rtts: 0,
            count_rtts: 0,
            prev_pkts: 0,
            prev_marks: 0,
            prev_losts: 0,
            rfc8888_ack: false,
            rfc8888_ackperiod: RFC8888_ACKPERIOD,
            rt_mode: false,
            rt_fps: FRAME_PER_SECOND,
            rt_frameduration: FRAME_DURATION,
        };

        app.parse_args(args)?;
        app.print_info();
        Ok(app)
    }

    #[inline]
    fn missing_value(flag: &'static str) -> AppError {
        AppError::MissingValue(flag)
    }

    #[inline]
    fn invalid_value(detail: &'static str) -> AppError {
        AppError::InvalidValue(detail)
    }

    /// Extract the runtime configuration used by the sender/receiver loops.
    pub fn runner_config(&self) -> RunnerConfig {
        RunnerConfig::from(self)
    }

    #[inline]
    fn record_timing_sample(&mut self, sample: time_tp) {
        self.acc_rtts = self.acc_rtts.saturating_add(sample.max(0) as u64);
        self.count_rtts = self.count_rtts.wrapping_add(1);
    }
}
