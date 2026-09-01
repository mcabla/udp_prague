// SPDX-License-Identifier: Apache-2.0
//
// Runtime/reporting glue is adapted from the Apache-2.0 UDP Prague example and
// exposes telemetry from the GPL-2.0-only Classic-AQM monitor.

//! Core runtime configuration and reporting hooks.
//!
//! These types are part of the reusable UDP Prague library surface and are
//! intentionally independent from the demo application's CLI/config/reporting
//! adapter in `demo::app`.

use crate::congestion::{
    count_tp, fps_tp, rate_tp, size_tp, time_tp, ClassicAqmAssessment, PRAGUE_INITMTU,
    PRAGUE_MAXRATE,
};

/// Default RFC8888 ACK period (µs).
pub const RFC8888_ACKPERIOD: u32 = 25_000;
/// Default FPS for RT mode.
pub const FRAME_PER_SECOND: fps_tp = 60;
/// Default frame duration (µs).
pub const FRAME_DURATION: u32 = 10_000;
/// Default UDP port.
pub const PORT: u16 = 8080;

/// Runtime configuration consumed by the sender/receiver loops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunnerConfig {
    /// Receiver address.
    pub rcv_addr: String,
    /// Receiver port.
    pub rcv_port: u32,
    /// Whether to `connect()` rather than `bind()`.
    pub connect: bool,
    /// Optional timeout for the non-connected sender startup trigger wait (µs).
    ///
    /// `None` preserves reference parity and waits indefinitely.
    pub startup_wait_timeout_us: Option<u32>,
    /// Max packet size.
    pub max_pkt: size_tp,
    /// Max pacing rate.
    pub max_rate: rate_tp,
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
    /// Whether the RFC 9331 Classic-AQM response is enabled.
    ///
    /// The passive detector remains active for telemetry when this is false,
    /// but its alpha floor is not applied to congestion response.
    pub classic_aqm_fallback_enabled: bool,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            rcv_addr: "0.0.0.0".to_string(),
            rcv_port: u32::from(PORT),
            connect: false,
            startup_wait_timeout_us: None,
            max_pkt: PRAGUE_INITMTU,
            max_rate: PRAGUE_MAXRATE,
            rfc8888_ack: false,
            rfc8888_ackperiod: RFC8888_ACKPERIOD,
            rt_mode: false,
            rt_fps: FRAME_PER_SECOND,
            rt_frameduration: FRAME_DURATION,
            classic_aqm_fallback_enabled: true,
        }
    }
}

/// ACK/loss counters reported by the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueAckCounters {
    pub packets_received: count_tp,
    pub packets_ce: count_tp,
    pub packets_lost: count_tp,
    pub error_l4s: bool,
}

/// Sender-side packet pacing/window state attached to reporter events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PraguePacketWindowMetrics {
    pub pacing_rate: rate_tp,
    pub packet_window: count_tp,
    pub packet_burst: count_tp,
    pub packet_inflight: count_tp,
    pub packet_inburst: count_tp,
    pub next_send: time_tp,
}

/// Sender-side frame/window state attached to RT reporter events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueFrameWindowMetrics {
    pub frame_window: count_tp,
    pub frame_inflight: count_tp,
    pub frame_sending: bool,
    pub sent_frame: count_tp,
    pub lost_frame: count_tp,
    pub recv_frame: count_tp,
}

/// Reporter event for one transmitted bulk Prague data packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSendDataEvent {
    pub now: time_tp,
    pub timestamp: time_tp,
    pub echoed_timestamp: time_tp,
    pub seqnr: count_tp,
    pub pkt_size: size_tp,
    pub transport: PraguePacketWindowMetrics,
}

/// Reporter event for one transmitted RT/frame Prague packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSendFrameDataEvent {
    pub now: time_tp,
    pub timestamp: time_tp,
    pub echoed_timestamp: time_tp,
    pub seqnr: count_tp,
    pub pkt_size: size_tp,
    pub pacing_rate: rate_tp,
    pub frame_window: count_tp,
    pub frame_size: count_tp,
    pub packet_burst: count_tp,
    pub frame_inflight: count_tp,
    pub frame_sent: size_tp,
    pub packet_inburst: count_tp,
    pub next_send: time_tp,
}

/// Reporter event for one received Prague data packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueRecvDataEvent {
    pub now: time_tp,
    pub timestamp: time_tp,
    pub echoed_timestamp: time_tp,
    pub seqnr: count_tp,
    pub bytes_received: size_tp,
}

/// Reporter event for one transmitted classic ACK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSendAckEvent {
    pub now: time_tp,
    pub timestamp: time_tp,
    pub echoed_timestamp: time_tp,
    pub seqnr: count_tp,
    pub packet_size: size_tp,
    pub counters: PragueAckCounters,
}

/// Reporter event for one transmitted RFC8888 ACK packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSendRfc8888AckEvent<'a> {
    pub now: time_tp,
    pub seqnr: count_tp,
    pub packet_size: size_tp,
    pub begin_seq: count_tp,
    pub num_reports: u16,
    pub report: &'a [u8],
}

/// Reporter event for one received classic ACK packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueRecvAckEvent {
    pub now: time_tp,
    pub timestamp: time_tp,
    pub echoed_timestamp: time_tp,
    pub seqnr: count_tp,
    pub bytes_received: size_tp,
    pub counters: PragueAckCounters,
    pub transport: PraguePacketWindowMetrics,
    pub frames: PragueFrameWindowMetrics,
}

/// Reporter event for one received RFC8888 ACK packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueRecvRfc8888AckEvent<'a> {
    pub now: time_tp,
    pub seqnr: count_tp,
    pub bytes_received: size_tp,
    pub begin_seq: count_tp,
    pub num_reports: u16,
    pub num_rtt: u16,
    pub rtts: &'a [time_tp],
    pub counters: PragueAckCounters,
    pub transport: PraguePacketWindowMetrics,
    pub frames: PragueFrameWindowMetrics,
}

/// Sender-side Classic-AQM detector update.
///
/// This is emitted by the compatibility runner after a valid ACK/RFC8888
/// extent has been observed. It lets reporters expose the safety decision
/// without coupling the core runner to a particular logging format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueClassicAqmEvent {
    pub now: time_tp,
    pub latest_rtt_us: time_tp,
    pub min_rtt_us: time_tp,
    pub acked_delta: count_tp,
    pub ce_delta: count_tp,
    pub app_limited: bool,
    pub congestion_avoidance_stable: bool,
    pub assessment: ClassicAqmAssessment,
}

/// Reporting callbacks used by the sender/receiver loops.
pub trait Reporter {
    fn LogSendData(&mut self, _event: &PragueSendDataEvent) {}

    fn LogSendFrameData(&mut self, _event: &PragueSendFrameDataEvent) {}

    fn LogRecvData(&mut self, _event: &PragueRecvDataEvent) {}

    fn LogSendACK(&mut self, _event: &PragueSendAckEvent) {}

    fn LogSendRFC8888ACK(&mut self, _event: &PragueSendRfc8888AckEvent<'_>) {}

    fn LogRecvACK(&mut self, _event: &PragueRecvAckEvent) {}

    fn LogRecvRFC8888ACK(&mut self, _event: &PragueRecvRfc8888AckEvent<'_>) {}

    fn LogClassicAqm(&mut self, _event: &PragueClassicAqmEvent) {}
}
