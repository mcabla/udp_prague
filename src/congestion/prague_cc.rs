//! Prague congestion controller (L4S Prague) — near-literal port.

use core::cmp;
use std::sync::OnceLock;
use std::time::Instant;

use super::classic_aqm::{
    ClassicAqmAssessment, ClassicAqmMonitor, ClassicAqmObservation, ClassicAqmState,
};

/// Size in bytes.
pub type size_tp = u64;
/// Fractional window size in µBytes (to match time in µs, for easy bytes/sec calculations).
pub type window_tp = u64;
/// Rate in bytes/second.
pub type rate_tp = u64;
/// Timestamp or interval in microseconds.
///
/// This is a signed 32-bit value with wrap-around semantics. Use only for intervals between
/// two timestamps; the absolute reference is not meaningful.
pub type time_tp = i32;
/// Count in packets (or frames), signed to allow wrap-around comparison.
pub type count_tp = i32;
/// Frames per second (video mode). `0` must be used for bulk.
pub type fps_tp = u8;
/// Probability/fixed-point accumulator type.
pub type prob_tp = i64;

/// ECN field in the IP header (2 bits).
///
/// Only values 0–3 are valid, and `ecn_l4s_id` (0b01) and `ecn_ce` (0b11) are L4S-valid.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ecn_tp {
    ecn_not_ect = 0,
    ecn_l4s_id = 1,
    ecn_ect0 = 2,
    ecn_ce = 3,
}

/// Congestion-control state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum cs_tp {
    cs_init = 0,
    cs_cong_avoid = 1,
    cs_in_loss = 2,
    cs_in_cwr = 3,
}

/// Which CC algorithm is active.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum cca_tp {
    cca_prague_win = 0,
    cca_prague_rate = 1,
}

/// Prague initial window size (packets).
pub const PRAGUE_INITWIN: count_tp = 10;
/// Prague minimum MTU size.
pub const PRAGUE_MINMTU: size_tp = 150;
/// Prague initial MTU size.
pub const PRAGUE_INITMTU: size_tp = 1400;
/// Prague initial rate (12500 B/s ≈ 100 kbps).
pub const PRAGUE_INITRATE: rate_tp = 12_500;
/// Prague minimum rate (12500 B/s ≈ 100 kbps).
pub const PRAGUE_MINRATE: rate_tp = 12_500;
/// Prague maximum rate (12_500_000_000 B/s ≈ 100 Gbps).
pub const PRAGUE_MAXRATE: rate_tp = 12_500_000_000;

// Prague consts and methods (ported literally from the C++ reference)
const MIN_STEP: rate_tp = 7; // Minimally wait for 7 RTTs to try to increase faster
const RATE_STEP: rate_tp = 1_920_000; // per 1920kB/s pacing rate wait one RTT longer
const QUEUE_GROWTH: time_tp = 1_000; // target queue growth of 1000us = 1ms
const BURST_TIME: time_tp = 250; // 250us
const REF_RTT: time_tp = 25_000; // 25ms
const PROB_SHIFT: u8 = 20;
const MAX_PROB: prob_tp = 1i64 << PROB_SHIFT;
const ALPHA_SHIFT: u8 = 4; // >>4 divide by 16
const MIN_PKT_BURST: count_tp = 1;
const MIN_PKT_WIN: count_tp = 2;
const RATE_OFFSET: u8 = 3; // +/-3% for non-RT mode transfer during vrtt halves
const MIN_FRAME_WIN: count_tp = 2;

#[inline]
fn wrapping_sub_time(a: time_tp, b: time_tp) -> time_tp {
    a.wrapping_sub(b)
}
#[inline]
fn wrapping_sub_count(a: count_tp, b: count_tp) -> count_tp {
    a.wrapping_sub(b)
}

#[inline]
fn ecn_mask_ce(v: ecn_tp) -> ecn_tp {
    match (v as u8) & (ecn_tp::ecn_ce as u8) {
        0 => ecn_tp::ecn_not_ect,
        1 => ecn_tp::ecn_l4s_id,
        2 => ecn_tp::ecn_ect0,
        3 => ecn_tp::ecn_ce,
        _ => ecn_tp::ecn_not_ect, // unreachable due to mask
    }
}

/// Multiply two u64 values, right-shift, and saturate to `u64::MAX` on overflow.
///
/// This matches the behavior of the C++ `mul_64_64_shift()` helper.
#[inline]
fn mul_64_64_shift(left: u64, right: u64, shift: u32) -> u64 {
    if shift >= 128 {
        return 0;
    }
    let prod = (left as u128).wrapping_mul(right as u128);
    let shifted = prod >> shift;
    if shifted > u64::MAX as u128 {
        u64::MAX
    } else {
        shifted as u64
    }
}

/// Divide with rounding: `(a + divisor/2) / divisor`, saturating to `u64::MAX` on divisor=0 or overflow.
///
/// Matches the behavior of the C++ `div_64_64_round()` helper.
#[inline]
fn div_64_64_round(a: u64, divisor: u64) -> u64 {
    if divisor == 0 {
        return u64::MAX;
    }
    let dividend = (a as u128) + ((divisor >> 1) as u128);
    let q = dividend / (divisor as u128);
    if q > u64::MAX as u128 {
        u64::MAX
    } else {
        q as u64
    }
}

/// Internal process start used to construct a stable monotonic u32 tick counter.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();

#[inline]
fn process_now_u32_micros() -> u32 {
    let start = PROCESS_START.get_or_init(Instant::now);
    // Saturate at u128 then truncate to u32 to intentionally wrap at 2^32 µs.
    let us = start.elapsed().as_micros();
    us as u32
}

/// Complete Prague state and parameters.
///
/// This is exposed for logging and introspection.
#[derive(Clone, Copy, Debug)]
pub struct PragueState {
    /// Used to have a start time of 0.
    pub m_start_ref: time_tp,

    // parameters
    pub m_init_rate: rate_tp,
    pub m_init_window: window_tp,
    pub m_min_rate: rate_tp,
    pub m_max_rate: rate_tp,
    pub m_max_packet_size: size_tp,
    pub m_frame_interval: time_tp,
    pub m_frame_budget: time_tp,

    // both-end variables
    pub m_ts_remote: time_tp,
    /// Last reported RTT (only for stats).
    pub m_rtt: time_tp,
    /// Measured and smoothed RTT (EWMA factor 1/8).
    pub m_srtt: time_tp,
    /// Virtual RTT = max(srtt, ref_rtt).
    pub m_vrtt: time_tp,

    // receiver-end variables (to be echoed to sender)
    pub m_r_prev_ts: time_tp,
    pub m_r_packets_received: count_tp,
    pub m_r_packets_CE: count_tp,
    pub m_r_packets_lost: count_tp,
    pub m_r_error_L4S: bool,

    // sender-end variables
    pub m_cc_ts: time_tp,
    pub m_packets_received: count_tp,
    pub m_packets_CE: count_tp,
    pub m_packets_lost: count_tp,
    pub m_packets_sent: count_tp,
    pub m_error_L4S: bool,

    // alpha calculation previous state
    pub m_alpha_ts: time_tp,
    pub m_alpha_packets_received: count_tp,
    pub m_alpha_packets_CE: count_tp,
    pub m_alpha_packets_lost: count_tp,
    pub m_alpha_packets_sent: count_tp,

    // loss and recovery
    pub m_loss_ts: time_tp,
    pub m_loss_cca: cca_tp,
    pub m_lost_window: window_tp,
    pub m_lost_rate: rate_tp,
    pub m_lost_rtts_to_growth: count_tp,
    pub m_loss_packets_lost: count_tp,
    pub m_loss_packets_sent: count_tp,

    // cwr
    pub m_cwr_ts: time_tp,
    pub m_cwr_packets_sent: count_tp,

    // cc variables
    pub m_cc_state: cs_tp,
    pub m_cca_mode: cca_tp,
    pub m_rtts_to_growth: count_tp,
    pub m_alpha: prob_tp,
    pub m_pacing_rate: rate_tp,
    pub m_fractional_window: window_tp,
    pub m_packet_burst: count_tp,
    pub m_packet_size: size_tp,
    pub m_packet_window: count_tp,
}

impl Default for PragueState {
    fn default() -> Self {
        Self {
            m_start_ref: 0,
            m_init_rate: PRAGUE_INITRATE,
            m_init_window: 0,
            m_min_rate: PRAGUE_MINRATE,
            m_max_rate: PRAGUE_MAXRATE,
            m_max_packet_size: PRAGUE_INITMTU,
            m_frame_interval: 0,
            m_frame_budget: 0,
            m_ts_remote: 0,
            m_rtt: 0,
            m_srtt: 0,
            m_vrtt: 0,
            m_r_prev_ts: 0,
            m_r_packets_received: 0,
            m_r_packets_CE: 0,
            m_r_packets_lost: 0,
            m_r_error_L4S: false,
            m_cc_ts: 0,
            m_packets_received: 0,
            m_packets_CE: 0,
            m_packets_lost: 0,
            m_packets_sent: 0,
            m_error_L4S: false,
            m_alpha_ts: 0,
            m_alpha_packets_received: 0,
            m_alpha_packets_CE: 0,
            m_alpha_packets_lost: 0,
            m_alpha_packets_sent: 0,
            m_loss_ts: 0,
            m_loss_cca: cca_tp::cca_prague_win,
            m_lost_window: 0,
            m_lost_rate: 0,
            m_lost_rtts_to_growth: 0,
            m_loss_packets_lost: 0,
            m_loss_packets_sent: 0,
            m_cwr_ts: 0,
            m_cwr_packets_sent: 0,
            m_cc_state: cs_tp::cs_init,
            m_cca_mode: cca_tp::cca_prague_win,
            m_rtts_to_growth: 0,
            m_alpha: 0,
            m_pacing_rate: PRAGUE_INITRATE,
            m_fractional_window: 0,
            m_packet_burst: MIN_PKT_BURST,
            m_packet_size: PRAGUE_INITMTU,
            m_packet_window: MIN_PKT_WIN,
        }
    }
}

/// Coarse application-facing summary of the current congestion situation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PragueCongestionSignal {
    /// No active ECN, loss, or fallback signal is currently visible.
    Stable,
    /// The controller is reacting to ECN marks in the L4S path.
    EcnMarked,
    /// The controller is in loss recovery.
    LossRecovery,
    /// L4S has been disabled and packets should be sent without the L4S ECN id.
    L4sFallback,
}

/// Direction of meaningful bitrate change between two advice snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PragueBitrateAction {
    Increase,
    Hold,
    Decrease,
}

/// Application-facing pacing and congestion advice for non-frame traffic.
///
/// This is a Rust-native summary over the lower-level Prague state and
/// `GetCCInfo()` output. Embedding applications can poll it after processing
/// ACK/RFC8888 feedback to drive encoder bitrate, message batching, or media
/// quality adaptation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueRateAdvice {
    /// Current target pacing rate in bytes per second.
    pub pacing_rate_bytes_per_sec: rate_tp,
    /// Current packet window in packets.
    pub packet_window: count_tp,
    /// Current burst allowance in packets.
    pub packet_burst: count_tp,
    /// Current target packet size in bytes.
    pub packet_size_bytes: size_tp,
    /// ECN marking to use for the next send.
    pub next_send_ecn: ecn_tp,
    /// Current controller state.
    pub controller_state: cs_tp,
    /// Current Prague algorithm mode.
    pub controller_mode: cca_tp,
    /// Most recent RTT sample in microseconds.
    pub last_rtt_us: time_tp,
    /// Smoothed RTT in microseconds.
    pub smoothed_rtt_us: time_tp,
    /// Virtual RTT in microseconds.
    pub virtual_rtt_us: time_tp,
    /// Approximate recent ECN pressure as parts-per-million in `[0, 1_000_000]`.
    pub ce_ratio_ppm: u32,
    /// Raw Prague CE EWMA in parts per million. Kept as an explicit alias so
    /// callers do not mistake the safety-clamped value for wire ECN pressure.
    pub raw_alpha_ppm: u32,
    /// Effective alpha after the optional Classic-AQM safety floor.
    pub effective_alpha_ppm: u32,
    /// Classic-AQM-derived alpha floor in parts per million.
    pub classic_ecn_alpha_floor_ppm: u32,
    /// Linux-reference `classic_ecn` score (scaled by 1 << 24).
    pub classic_ecn_score: u64,
    /// Whether the monitor currently classifies the path as Classic-compatible.
    pub classic_ecn_fallback_active: bool,
    /// Whether Prague's independent L4S marking negotiation/error state is active.
    pub l4s_marking_error_active: bool,
    /// Coarse congestion summary for application adaptation logic.
    pub congestion_signal: PragueCongestionSignal,
    /// Legacy alias for the ECN-integrity/marking-error state. Classic-AQM
    /// compatibility is reported separately by `classic_ecn_fallback_active`.
    pub l4s_fallback_active: bool,
    /// Cumulative acknowledged packets observed by the sender-side controller.
    pub packets_received: count_tp,
    /// Cumulative CE-marked packets observed by the sender-side controller.
    pub packets_ce: count_tp,
    /// Cumulative lost packets observed by the sender-side controller.
    pub packets_lost: count_tp,
}

impl PragueRateAdvice {
    fn new(
        state: PragueState,
        classic: ClassicAqmAssessment,
        pacing_rate_bytes_per_sec: rate_tp,
        packet_window: count_tp,
        packet_burst: count_tp,
        packet_size_bytes: size_tp,
    ) -> Self {
        let raw_alpha_ppm = alpha_to_ppm(state.m_alpha);
        let effective_alpha = state.m_alpha.max(classic.alpha_floor as prob_tp);
        Self {
            pacing_rate_bytes_per_sec,
            packet_window,
            packet_burst,
            packet_size_bytes,
            // A transient Classic-AQM-compatible response keeps ECT(1).  This
            // follows Linux TCP-Prague's `prague_classic_ecn_fallback`: the
            // fallback clamps alpha for coexistence with a Classic AQM; it
            // does not change the L4S codepoint to ECT(0). See:
            // https://github.com/L4STeam/linux/blob/testing/net/ipv4/tcp_prague.c
            // and the supplied 6.18 comparison:
            // https://github.com/minuscat/l4steam-6.18.y/compare/linux-6.18.y...testing-net-next39
            // ECT(0) is reserved for an explicit administrative switch to a
            // Classic controller. A separate ECN-integrity error may select
            // Not-ECT, as required by the Prague safety fallback.
            next_send_ecn: if state.m_error_L4S {
                ecn_tp::ecn_not_ect
            } else {
                ecn_tp::ecn_l4s_id
            },
            controller_state: state.m_cc_state,
            controller_mode: state.m_cca_mode,
            last_rtt_us: state.m_rtt,
            smoothed_rtt_us: state.m_srtt,
            virtual_rtt_us: state.m_vrtt,
            ce_ratio_ppm: raw_alpha_ppm,
            raw_alpha_ppm,
            effective_alpha_ppm: alpha_to_ppm(effective_alpha),
            classic_ecn_alpha_floor_ppm: alpha_to_ppm(classic.alpha_floor as prob_tp),
            classic_ecn_score: classic.classic_ecn_score,
            classic_ecn_fallback_active: classic.state == ClassicAqmState::ClassicCompatible
                && classic.alpha_floor > 0,
            l4s_marking_error_active: state.m_error_L4S,
            congestion_signal: congestion_signal_from_state(&state),
            l4s_fallback_active: state.m_error_L4S,
            packets_received: state.m_packets_received,
            packets_ce: state.m_packets_CE,
            packets_lost: state.m_packets_lost,
        }
    }

    /// Return the pacing rate in bits per second for encoder-style APIs.
    pub fn pacing_rate_bits_per_sec(&self) -> u64 {
        self.pacing_rate_bytes_per_sec.saturating_mul(8)
    }

    /// Compare two advice snapshots and classify the bitrate change.
    ///
    /// `tolerance_percent` suppresses tiny changes. For example, passing `5`
    /// returns `Hold` for changes within ±5%.
    pub fn bitrate_action_since(
        &self,
        previous: &Self,
        tolerance_percent: u8,
    ) -> PragueBitrateAction {
        let tolerance = u64::from(tolerance_percent.min(100));
        let previous_rate = previous.pacing_rate_bytes_per_sec.max(1);
        let increase_threshold = previous_rate.saturating_mul(100 + tolerance) / 100;
        let decrease_threshold = previous_rate.saturating_mul(100 - tolerance) / 100;

        if self.pacing_rate_bytes_per_sec > increase_threshold {
            PragueBitrateAction::Increase
        } else if self.pacing_rate_bytes_per_sec < decrease_threshold {
            PragueBitrateAction::Decrease
        } else {
            PragueBitrateAction::Hold
        }
    }
}

/// Application-facing advice for frame-oriented traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueVideoRateAdvice {
    /// Transport-level Prague guidance shared with non-frame traffic.
    pub transport: PragueRateAdvice,
    /// Target frame size in bytes.
    pub target_frame_size_bytes: size_tp,
    /// Maximum number of frames that should be in flight.
    pub frame_window: count_tp,
}

impl PragueVideoRateAdvice {
    /// Return the pacing rate in bits per second for encoder-style APIs.
    pub fn pacing_rate_bits_per_sec(&self) -> u64 {
        self.transport.pacing_rate_bits_per_sec()
    }

    /// Compare two frame-advice snapshots and classify the bitrate change.
    pub fn bitrate_action_since(
        &self,
        previous: &Self,
        tolerance_percent: u8,
    ) -> PragueBitrateAction {
        self.transport
            .bitrate_action_since(&previous.transport, tolerance_percent)
    }
}

#[inline]
fn alpha_to_ppm(alpha: prob_tp) -> u32 {
    let alpha = alpha.clamp(0, MAX_PROB) as u128;
    ((alpha * 1_000_000u128) / (MAX_PROB as u128)) as u32
}

#[inline]
fn congestion_signal_from_state(state: &PragueState) -> PragueCongestionSignal {
    if state.m_error_L4S {
        PragueCongestionSignal::L4sFallback
    } else if state.m_cc_state == cs_tp::cs_in_loss {
        PragueCongestionSignal::LossRecovery
    } else if state.m_cc_state == cs_tp::cs_in_cwr || state.m_alpha > 0 {
        PragueCongestionSignal::EcnMarked
    } else {
        PragueCongestionSignal::Stable
    }
}

/// Prague congestion controller.
///
/// This is a near-literal port of the C++ `PragueCC` class. Method names intentionally
/// match the original for easier cross-referencing.
#[derive(Clone)]
pub struct PragueCC {
    state: PragueState,
    classic_aqm: ClassicAqmMonitor,
    classic_aqm_fallback_enabled: bool,
}

impl Default for PragueCC {
    /// Construct a PragueCC instance using the default parameters from the reference code.
    fn default() -> Self {
        Self::new(
            PRAGUE_INITMTU,
            0,
            0,
            PRAGUE_INITRATE,
            PRAGUE_INITWIN,
            PRAGUE_MINRATE,
            PRAGUE_MAXRATE,
        )
    }
}

impl PragueCC {
    /// Construct a new Prague congestion controller.
    ///
    /// Arguments mirror the C++ constructor (which had default arguments).
    pub fn new(
        max_packet_size: size_tp,
        fps: fps_tp,
        frame_budget: time_tp,
        init_rate: rate_tp,
        init_window: count_tp,
        min_rate: rate_tp,
        max_rate: rate_tp,
    ) -> Self {
        let mut cc = Self {
            state: PragueState {
                m_start_ref: 0,
                ..PragueState::default()
            },
            classic_aqm: ClassicAqmMonitor::default(),
            classic_aqm_fallback_enabled: true,
        };
        let ts_now = cc.Now();

        // parameters
        cc.state.m_init_rate = init_rate;
        cc.state.m_init_window = (init_window as window_tp)
            .wrapping_mul(max_packet_size)
            .wrapping_mul(1_000_000);
        cc.state.m_min_rate = min_rate;
        cc.state.m_max_rate = max_rate;
        cc.state.m_max_packet_size = max_packet_size;
        cc.state.m_frame_interval = if fps != 0 {
            (1_000_000u32 / fps as u32) as time_tp
        } else {
            0
        };
        cc.state.m_frame_budget = frame_budget;
        if cc.state.m_frame_budget > cc.state.m_frame_interval {
            cc.state.m_frame_budget = cc.state.m_frame_interval;
        }

        // both-end
        cc.state.m_ts_remote = 0;
        cc.state.m_rtt = 0;
        cc.state.m_srtt = 0;
        cc.state.m_vrtt = 0;

        // receiver-end
        cc.state.m_r_prev_ts = 0;
        cc.state.m_r_packets_received = 0;
        cc.state.m_r_packets_CE = 0;
        cc.state.m_r_packets_lost = 0;
        cc.state.m_r_error_L4S = false;

        // sender-end
        cc.state.m_cc_ts = ts_now;
        cc.state.m_packets_received = 0;
        cc.state.m_packets_CE = 0;
        cc.state.m_packets_lost = 0;
        cc.state.m_packets_sent = 0;
        cc.state.m_error_L4S = false;

        // alpha
        cc.state.m_alpha_ts = ts_now;
        cc.state.m_alpha_packets_received = 0;
        cc.state.m_alpha_packets_CE = 0;
        cc.state.m_alpha_packets_lost = 0;
        cc.state.m_alpha_packets_sent = 0;

        // loss and recovery
        cc.state.m_loss_ts = 0;
        cc.state.m_loss_cca = cca_tp::cca_prague_win;
        cc.state.m_lost_window = 0;
        cc.state.m_lost_rate = 0;
        cc.state.m_loss_packets_lost = 0;
        cc.state.m_loss_packets_sent = 0;
        cc.state.m_lost_rtts_to_growth = 0;

        // cwr
        cc.state.m_cwr_ts = 0;
        cc.state.m_cwr_packets_sent = 0;

        // cc variables
        cc.state.m_cc_state = cs_tp::cs_init;
        cc.state.m_cca_mode = cca_tp::cca_prague_win;
        cc.state.m_rtts_to_growth = (init_rate / RATE_STEP) as count_tp + (MIN_STEP as count_tp);
        cc.state.m_alpha = 0;
        cc.state.m_pacing_rate = init_rate;
        cc.state.m_fractional_window = cc.state.m_init_window;

        // determine packet size
        let ref_rtt = cc.get_ref_rtt();
        cc.state.m_packet_size =
            cc.state.m_pacing_rate.wrapping_mul(ref_rtt as u64) / 1_000_000 / (MIN_PKT_WIN as u64);
        if cc.state.m_packet_size < PRAGUE_MINMTU {
            cc.state.m_packet_size = PRAGUE_MINMTU;
        }
        if cc.state.m_packet_size > cc.state.m_max_packet_size {
            cc.state.m_packet_size = cc.state.m_max_packet_size;
        }

        // packet burst
        cc.state.m_packet_burst = (cc.state.m_pacing_rate.wrapping_mul(BURST_TIME as u64)
            / 1_000_000
            / cc.state.m_packet_size) as count_tp;
        if cc.state.m_packet_burst < MIN_PKT_BURST {
            cc.state.m_packet_burst = MIN_PKT_BURST;
        }

        // packet window
        cc.state.m_packet_window =
            (cc.state.m_fractional_window / 1_000_000).div_ceil(cc.state.m_packet_size) as count_tp;
        if cc.state.m_packet_window < MIN_PKT_WIN {
            cc.state.m_packet_window = MIN_PKT_WIN;
        }

        cc
    }

    /// Update the maximum packet size used by the controller after a QUIC
    /// path-MTU change or migration. The algorithm keeps its current packet
    /// size bounded by the new maximum; all other Prague state is preserved.
    pub fn set_max_packet_size(&mut self, max_packet_size: size_tp) {
        self.state.m_max_packet_size = max_packet_size.max(1);
        self.state.m_packet_size = self.state.m_packet_size.min(self.state.m_max_packet_size);
    }

    /// Enable/disable the RFC 9331 Classic-AQM response while retaining the
    /// detector and its telemetry. Disabled mode is useful for controlled
    /// experiments; safety-enabled profiles should leave it enabled.
    pub fn set_classic_aqm_fallback_enabled(&mut self, enabled: bool) {
        self.classic_aqm_fallback_enabled = enabled;
    }

    pub fn classic_aqm_fallback_enabled(&self) -> bool {
        self.classic_aqm_fallback_enabled
    }

    pub fn observe_classic_aqm(
        &mut self,
        observation: ClassicAqmObservation,
    ) -> ClassicAqmAssessment {
        self.classic_aqm.observe(observation)
    }

    pub fn classic_aqm_assessment(&self) -> ClassicAqmAssessment {
        self.classic_aqm.assessment()
    }

    /// Whether the adapter has a stable congestion-avoidance signal suitable
    /// for passive AQM inference.
    pub fn congestion_avoidance_stable(&self) -> bool {
        self.state.m_cc_state == cs_tp::cs_cong_avoid
    }

    #[inline]
    fn effective_alpha(&self) -> prob_tp {
        if self.classic_aqm_fallback_enabled {
            self.state
                .m_alpha
                .max(self.classic_aqm.alpha_floor() as prob_tp)
        } else {
            self.state.m_alpha
        }
    }

    fn advice_assessment(&self) -> ClassicAqmAssessment {
        let mut assessment = self.classic_aqm.assessment();
        if !self.classic_aqm_fallback_enabled {
            assessment.alpha_floor = 0;
        }
        assessment
    }

    /// Get a const pointer to the internal state (for logging).
    pub fn GetStatePtr(&self) -> &PragueState {
        &self.state
    }

    /// Copy internal state (for logging).
    pub fn GetStats(&self, stats: &mut PragueState) {
        *stats = self.state;
    }

    /// Can be overwritten in the C++ reference; here it is a method returning a wrapping i32 tick.
    ///
    /// Returns a monotonic increasing signed i32 in microseconds, wrapping at 2^32 µs, skipping 0.
    pub fn Now(&mut self) -> time_tp {
        // C++ behavior: first call sets a reference and returns 1, ensuring `now != 0`.
        if self.state.m_start_ref == 0 {
            let mut start = process_now_u32_micros() as time_tp;
            if start == 0 {
                start = -1; // avoid next now being <= this value
            }
            self.state.m_start_ref = start;
            return 1;
        }

        let now_abs = process_now_u32_micros() as time_tp;
        let now = wrapping_sub_time(now_abs, self.state.m_start_ref);
        if now == 0 {
            1
        } else {
            now
        }
    }

    /// Reference RTT used when `m_frame_interval == 0`.
    pub fn get_ref_rtt(&self) -> time_tp {
        if self.state.m_frame_interval != 0 {
            self.state.m_frame_interval
        } else {
            REF_RTT
        }
    }

    /// Shift factor used for alpha EWMA.
    pub fn get_alpha_shift(&self) -> count_tp {
        if self.state.m_frame_interval != 0 {
            (((1u64 << ALPHA_SHIFT) * (REF_RTT as u64)) / (self.state.m_frame_interval as u64))
                as count_tp
        } else {
            1i32 << ALPHA_SHIFT
        }
    }

    /// Feed RFC8888 RTT samples (block ACK mode) into the RTT estimator.
    pub fn RFC8888Received(&mut self, num_rtt: usize, pkts_rtt: &[time_tp]) -> bool {
        let n = cmp::min(num_rtt, pkts_rtt.len());
        for &rtt in &pkts_rtt[..n] {
            self.state.m_rtt = rtt;
            if self.state.m_cc_state != cs_tp::cs_init {
                self.state.m_srtt = self
                    .state
                    .m_srtt
                    .wrapping_add((self.state.m_rtt.wrapping_sub(self.state.m_srtt)) >> 3);
            } else {
                self.state.m_srtt = self.state.m_rtt;
            }
            let ref_rtt = self.get_ref_rtt();
            self.state.m_vrtt = if self.state.m_srtt > ref_rtt {
                self.state.m_srtt
            } else {
                ref_rtt
            };
        }
        true
    }

    /// Call when a packet is received from peer.
    ///
    /// Returns `false` if an older packet is ignored.
    pub fn PacketReceived(&mut self, timestamp: time_tp, echoed_timestamp: time_tp) -> bool {
        // Ignore older packets (timestamp can't go backwards).
        if self.state.m_cc_state != cs_tp::cs_init
            && wrapping_sub_time(self.state.m_r_prev_ts, timestamp) > 0
        {
            return false;
        }

        let ts = self.Now();
        self.state.m_ts_remote = wrapping_sub_time(ts, timestamp);
        self.state.m_rtt = wrapping_sub_time(ts, echoed_timestamp);
        if self.state.m_cc_state != cs_tp::cs_init {
            self.state.m_srtt = self
                .state
                .m_srtt
                .wrapping_add((self.state.m_rtt.wrapping_sub(self.state.m_srtt)) >> 3);
        } else {
            self.state.m_srtt = self.state.m_rtt;
        }
        let ref_rtt = self.get_ref_rtt();
        self.state.m_vrtt = if self.state.m_srtt > ref_rtt {
            self.state.m_srtt
        } else {
            ref_rtt
        };
        self.state.m_r_prev_ts = timestamp;
        true
    }

    /// Call when an ACK is received from peer.
    ///
    /// Returns `false` if the ACK is older/invalid and ignored.
    pub fn ACKReceived(
        &mut self,
        packets_received: count_tp,
        packets_CE: count_tp,
        packets_lost: count_tp,
        packets_sent: count_tp,
        error_L4S: bool,
        inflight: &mut count_tp,
    ) -> bool {
        // Ignore older/invalid ACKs (counters can't go down in new ACKs)
        if wrapping_sub_count(self.state.m_packets_received, packets_received) > 0
            || wrapping_sub_count(self.state.m_packets_CE, packets_CE) > 0
        {
            return false;
        }

        // select rate- or window-based update
        let pacing_interval: time_tp = (self.state.m_packet_size.wrapping_mul(1_000_000)
            / cmp::max(self.state.m_pacing_rate, 1))
            as time_tp;
        let srtt: time_tp = self.state.m_srtt;

        // initialize window with initial pacing rate
        if self.state.m_cc_state == cs_tp::cs_init {
            self.state.m_fractional_window = (srtt as u64).wrapping_mul(self.state.m_pacing_rate);
            self.state.m_cc_state = cs_tp::cs_cong_avoid;
        }

        // below pacing interval or 2ms the RTT is too unstable
        if srtt <= 2_000 || srtt <= pacing_interval {
            self.state.m_cca_mode = cca_tp::cca_prague_rate;
        } else {
            if self.state.m_cca_mode == cca_tp::cca_prague_rate {
                self.state.m_fractional_window =
                    (srtt as u64).wrapping_mul(self.state.m_pacing_rate);
            }
            self.state.m_cca_mode = cca_tp::cca_prague_win;
        }

        let ts = self.Now();

        // Update alpha if both a window and a virtual rtt are passed
        if wrapping_sub_count(
            packets_received.wrapping_add(packets_lost),
            self.state.m_alpha_packets_sent,
        ) > 0
            && (wrapping_sub_time(ts, self.state.m_alpha_ts).wrapping_sub(self.state.m_vrtt) >= 0)
        {
            let denom = packets_received.wrapping_sub(self.state.m_alpha_packets_received);
            if denom != 0 {
                let num = (packets_CE.wrapping_sub(self.state.m_alpha_packets_CE) as prob_tp)
                    << PROB_SHIFT;
                let prob = num / (denom as prob_tp);
                let alpha_shift = self.get_alpha_shift() as prob_tp;
                if alpha_shift != 0 {
                    self.state.m_alpha += (prob - self.state.m_alpha) / alpha_shift;
                }
                if self.state.m_alpha > MAX_PROB {
                    self.state.m_alpha = MAX_PROB;
                }
            }
            self.state.m_alpha_packets_sent = packets_sent;
            self.state.m_alpha_packets_CE = packets_CE;
            self.state.m_alpha_packets_received = packets_received;
            self.state.m_alpha_ts = ts;
            if self.state.m_rtts_to_growth > 0 {
                self.state.m_rtts_to_growth -= 1;
            }
        }

        // Undo window reduction if loss count is down again (reordering)
        if (self.state.m_lost_window > 0 || self.state.m_lost_rate > 0)
            && (wrapping_sub_count(self.state.m_loss_packets_lost, packets_lost) >= 0)
        {
            self.state.m_cca_mode = self.state.m_loss_cca;
            if self.state.m_cca_mode == cca_tp::cca_prague_rate {
                self.state.m_pacing_rate = self
                    .state
                    .m_pacing_rate
                    .wrapping_add(self.state.m_lost_rate);
                self.state.m_lost_rate = 0;
            } else {
                self.state.m_fractional_window = self
                    .state
                    .m_fractional_window
                    .wrapping_add(self.state.m_lost_window);
                self.state.m_lost_window = 0;
            }
            self.state.m_rtts_to_growth = self
                .state
                .m_rtts_to_growth
                .wrapping_sub(self.state.m_lost_rtts_to_growth);
            if self.state.m_rtts_to_growth < 0 {
                self.state.m_rtts_to_growth = 0;
            }
            self.state.m_lost_rtts_to_growth = 0;
            self.state.m_cc_state = cs_tp::cs_cong_avoid;
        }

        // Clear in_loss state if in_loss and a real and virtual rtt are passed
        if self.state.m_cc_state == cs_tp::cs_in_loss
            && wrapping_sub_count(
                packets_received.wrapping_add(packets_lost),
                self.state.m_loss_packets_sent,
            ) > 0
            && (wrapping_sub_time(ts, self.state.m_loss_ts).wrapping_sub(self.state.m_vrtt) >= 0)
        {
            self.state.m_cc_state = cs_tp::cs_cong_avoid;
        }

        // Reduce window if loss count increased
        if self.state.m_cc_state != cs_tp::cs_in_loss
            && wrapping_sub_count(self.state.m_packets_lost, packets_lost) < 0
        {
            // vRTTs needed to get to time where a REF_RTT flow would hit same bottleneck.
            let mut rtts_to_growth_u64 = self.state.m_pacing_rate / 2;
            rtts_to_growth_u64 /= cmp::max(self.state.m_max_packet_size, 1);
            rtts_to_growth_u64 = rtts_to_growth_u64.saturating_mul(REF_RTT as u64);
            rtts_to_growth_u64 /= cmp::max(self.state.m_vrtt as u64, 1);
            rtts_to_growth_u64 = rtts_to_growth_u64.saturating_mul(REF_RTT as u64);
            rtts_to_growth_u64 /= 1_000_000;
            let rtts_to_growth = rtts_to_growth_u64 as count_tp;

            self.state.m_lost_rtts_to_growth += rtts_to_growth - self.state.m_rtts_to_growth;
            if self.state.m_lost_rtts_to_growth > rtts_to_growth {
                self.state.m_lost_rtts_to_growth = rtts_to_growth;
            }
            self.state.m_rtts_to_growth = rtts_to_growth;

            if self.state.m_cca_mode == cca_tp::cca_prague_win {
                self.state.m_lost_window = self.state.m_fractional_window / 2;
                self.state.m_fractional_window = self
                    .state
                    .m_fractional_window
                    .wrapping_sub(self.state.m_lost_window);
            } else {
                self.state.m_lost_rate = self.state.m_pacing_rate / 2;
                self.state.m_pacing_rate = self
                    .state
                    .m_pacing_rate
                    .wrapping_sub(self.state.m_lost_rate);
            }

            self.state.m_cc_state = cs_tp::cs_in_loss;
            self.state.m_loss_cca = self.state.m_cca_mode;
            self.state.m_loss_packets_sent = packets_sent;
            self.state.m_loss_ts = ts;
            self.state.m_loss_packets_lost = self.state.m_packets_lost;
        }

        // Increase window if not in_loss for all the non-CE ACKs
        let acks = (packets_received.wrapping_sub(self.state.m_packets_received))
            .wrapping_sub(packets_CE.wrapping_sub(self.state.m_packets_CE));
        if self.state.m_cc_state != cs_tp::cs_in_loss && acks > 0 {
            let mut increment =
                mul_64_64_shift(self.state.m_pacing_rate, QUEUE_GROWTH as u64, 0) / 1_000_000;
            if increment < self.state.m_max_packet_size || self.state.m_rtts_to_growth != 0 {
                increment = self.state.m_max_packet_size;
            }

            if self.state.m_cca_mode == cca_tp::cca_prague_win {
                let divisor =
                    mul_64_64_shift(self.state.m_vrtt as u64, self.state.m_vrtt as u64, 0);
                let scaler = div_64_64_round(
                    (srtt as u64)
                        .wrapping_mul(1_000_000)
                        .wrapping_mul(srtt as u64),
                    divisor,
                );
                let increase = div_64_64_round(
                    (acks as u64)
                        .wrapping_mul(self.state.m_packet_size)
                        .wrapping_mul(scaler)
                        .wrapping_mul(1_000_000),
                    cmp::max(self.state.m_fractional_window, 1),
                );
                let scaled_increase = mul_64_64_shift(increase, increment, 0);
                self.state.m_fractional_window =
                    self.state.m_fractional_window.wrapping_add(scaled_increase);
            } else {
                let divisor = mul_64_64_shift(self.state.m_packet_size, 1_000_000, 0);
                let invscaler = div_64_64_round(
                    mul_64_64_shift(self.state.m_pacing_rate, self.state.m_vrtt as u64, 0),
                    divisor,
                );
                let increase = div_64_64_round(
                    mul_64_64_shift((acks as u64).wrapping_mul(increment), 1_000_000, 0),
                    cmp::max(self.state.m_vrtt as u64, 1),
                );
                let scaled_increase = div_64_64_round(increase, cmp::max(invscaler, 1));
                self.state.m_pacing_rate = self.state.m_pacing_rate.wrapping_add(scaled_increase);
            }
        }

        // Clear in_cwr state if in_cwr and a real and virtual rtt are passed
        if self.state.m_cc_state == cs_tp::cs_in_cwr
            && wrapping_sub_count(
                packets_received.wrapping_add(packets_lost),
                self.state.m_cwr_packets_sent,
            ) > 0
            && (wrapping_sub_time(ts, self.state.m_cwr_ts).wrapping_sub(self.state.m_vrtt) >= 0)
        {
            self.state.m_cc_state = cs_tp::cs_cong_avoid;
        }

        // Reduce window if CE count increased and not in loss/cwr
        if self.state.m_cc_state == cs_tp::cs_cong_avoid
            && wrapping_sub_count(self.state.m_packets_CE, packets_CE) < 0
        {
            self.state.m_rtts_to_growth =
                (self.state.m_pacing_rate / RATE_STEP) as count_tp + (MIN_STEP as count_tp);
            let alpha_u64 = self.effective_alpha() as u64;
            if self.state.m_cca_mode == cca_tp::cca_prague_win {
                self.state.m_fractional_window = self.state.m_fractional_window.wrapping_sub(
                    (self.state.m_fractional_window.wrapping_mul(alpha_u64)) >> (PROB_SHIFT + 1),
                );
            } else {
                self.state.m_pacing_rate = self.state.m_pacing_rate.wrapping_sub(
                    (self.state.m_pacing_rate.wrapping_mul(alpha_u64)) >> (PROB_SHIFT + 1),
                );
            }

            self.state.m_cc_state = cs_tp::cs_in_cwr;
            self.state.m_cwr_packets_sent = packets_sent;
            self.state.m_cwr_ts = ts;
        }

        // Updating dependent parameters
        if self.state.m_cca_mode != cca_tp::cca_prague_rate {
            self.state.m_pacing_rate = self.state.m_fractional_window / cmp::max(srtt as u64, 1);
        }
        if self.state.m_pacing_rate < self.state.m_min_rate {
            self.state.m_pacing_rate = self.state.m_min_rate;
        }
        if self.state.m_pacing_rate > self.state.m_max_rate {
            self.state.m_pacing_rate = self.state.m_max_rate;
        }
        self.state.m_fractional_window = self.state.m_pacing_rate.wrapping_mul(srtt as u64);
        if self.state.m_fractional_window == 0 {
            self.state.m_fractional_window = 1;
        }

        // determine packet size
        self.state.m_packet_size = self
            .state
            .m_pacing_rate
            .wrapping_mul(self.state.m_vrtt as u64)
            / 1_000_000
            / (MIN_PKT_WIN as u64);
        if self.state.m_packet_size < PRAGUE_MINMTU {
            self.state.m_packet_size = PRAGUE_MINMTU;
        }
        if self.state.m_packet_size > self.state.m_max_packet_size {
            self.state.m_packet_size = self.state.m_max_packet_size;
        }

        // packet burst
        self.state.m_packet_burst = (self.state.m_pacing_rate.wrapping_mul(BURST_TIME as u64)
            / 1_000_000
            / cmp::max(self.state.m_packet_size, 1))
            as count_tp;
        if self.state.m_packet_burst < MIN_PKT_BURST {
            self.state.m_packet_burst = MIN_PKT_BURST;
        }

        // packet window: allow 3% higher pacing rate and round up
        self.state.m_packet_window =
            (((self.state.m_fractional_window * (100 + RATE_OFFSET as u64)) / 100_000_000)
                / cmp::max(self.state.m_packet_size, 1)
                + 1) as count_tp;
        if self.state.m_packet_window < MIN_PKT_WIN {
            self.state.m_packet_window = MIN_PKT_WIN;
        }

        // remember previous ACK for next ACK
        self.state.m_cc_ts = ts;
        self.state.m_packets_received = packets_received;
        self.state.m_packets_CE = packets_CE;
        self.state.m_packets_lost = packets_lost;
        self.state.m_packets_sent = packets_sent;
        if error_L4S {
            self.state.m_error_L4S = true;
        }
        *inflight = packets_sent
            .wrapping_sub(self.state.m_packets_received)
            .wrapping_sub(self.state.m_packets_lost);
        true
    }

    /// Call when a data packet with a sequence number is received (receiver side).
    pub fn DataReceivedSequence(&mut self, mut ip_ecn: ecn_tp, packet_seq_nr: count_tp) {
        ip_ecn = ecn_mask_ce(ip_ecn);
        self.state.m_r_packets_received = self.state.m_r_packets_received.wrapping_add(1);
        let skipped = packet_seq_nr
            .wrapping_sub(self.state.m_r_packets_received)
            .wrapping_sub(self.state.m_r_packets_lost);
        if skipped >= 0 {
            self.state.m_r_packets_lost = self.state.m_r_packets_lost.wrapping_add(skipped);
        } else if self.state.m_r_packets_lost > 0 {
            self.state.m_r_packets_lost = self.state.m_r_packets_lost.wrapping_sub(1);
        }
        if ip_ecn == ecn_tp::ecn_ce {
            self.state.m_r_packets_CE = self.state.m_r_packets_CE.wrapping_add(1);
        } else if ip_ecn != ecn_tp::ecn_l4s_id {
            self.state.m_r_error_L4S = true;
        }
    }

    /// Call when a data packet is received and lost packets can be identified (receiver side).
    pub fn DataReceived(&mut self, mut ip_ecn: ecn_tp, packets_lost: count_tp) {
        ip_ecn = ecn_mask_ce(ip_ecn);
        self.state.m_r_packets_received = self.state.m_r_packets_received.wrapping_add(1);
        self.state.m_r_packets_lost = self.state.m_r_packets_lost.wrapping_add(packets_lost);
        if ip_ecn == ecn_tp::ecn_ce {
            self.state.m_r_packets_CE = self.state.m_r_packets_CE.wrapping_add(1);
        } else if ip_ecn != ecn_tp::ecn_l4s_id {
            self.state.m_r_error_L4S = true;
        }
    }

    /// Reset CC state (e.g., after an RTO).
    pub fn ResetCCInfo(&mut self) {
        self.state.m_cc_ts = self.Now();
        self.state.m_cc_state = cs_tp::cs_init;
        self.state.m_cca_mode = cca_tp::cca_prague_win;
        self.state.m_alpha_ts = self.state.m_cc_ts;
        self.state.m_alpha = 0;
        self.state.m_pacing_rate = self.state.m_init_rate;
        self.state.m_fractional_window = self.state.m_max_packet_size.wrapping_mul(1_000_000);
        self.state.m_packet_burst = MIN_PKT_BURST;
        self.state.m_packet_size = self.state.m_max_packet_size;
        self.state.m_packet_window = MIN_PKT_WIN;
        self.state.m_rtts_to_growth =
            (self.state.m_pacing_rate / RATE_STEP) as count_tp + (MIN_STEP as count_tp);
        self.state.m_lost_rtts_to_growth = 0;
        self.classic_aqm.reset();
    }

    /// Get time/ECN information to attach to outgoing packets.
    pub fn GetTimeInfo(
        &mut self,
        timestamp: &mut time_tp,
        echoed_timestamp: &mut time_tp,
        ip_ecn: &mut ecn_tp,
    ) {
        *timestamp = self.Now();
        if self.state.m_ts_remote != 0 {
            *echoed_timestamp = timestamp.wrapping_sub(self.state.m_ts_remote);
        } else {
            *echoed_timestamp = 0;
        }
        *ip_ecn = if self.state.m_error_L4S {
            ecn_tp::ecn_not_ect
        } else {
            ecn_tp::ecn_l4s_id
        };
    }

    /// Get CC information for sending packets.
    pub fn GetCCInfo(
        &mut self,
        pacing_rate: &mut rate_tp,
        packet_window: &mut count_tp,
        packet_burst: &mut count_tp,
        packet_size: &mut size_tp,
    ) {
        if wrapping_sub_time(self.Now(), self.state.m_alpha_ts).wrapping_sub(self.state.m_vrtt >> 1)
            >= 0
        {
            *pacing_rate = self.state.m_pacing_rate * 100 / (100 + RATE_OFFSET as u64);
        } else {
            *pacing_rate = self.state.m_pacing_rate * (100 + RATE_OFFSET as u64) / 100;
        }
        *packet_window = self.state.m_packet_window;
        *packet_burst = self.state.m_packet_burst;
        *packet_size = self.state.m_packet_size;
    }

    /// Get CC information for video frames.
    pub fn GetCCInfoVideo(
        &mut self,
        pacing_rate: &mut rate_tp,
        frame_size: &mut size_tp,
        frame_window: &mut count_tp,
        packet_burst: &mut count_tp,
        packet_size: &mut size_tp,
    ) {
        *pacing_rate = self.state.m_pacing_rate;
        *packet_burst = self.state.m_packet_burst;
        *packet_size = self.state.m_packet_size;
        let fs = self
            .state
            .m_pacing_rate
            .wrapping_mul(self.state.m_frame_budget as u64)
            / 1_000_000;
        *frame_size = if self.state.m_packet_size > fs {
            self.state.m_packet_size
        } else {
            fs
        };
        *frame_window = (self.state.m_packet_window as i64 * self.state.m_packet_size as i64
            / (*frame_size as i64)) as count_tp;
        if *frame_window < MIN_FRAME_WIN {
            *frame_window = MIN_FRAME_WIN;
        }
    }

    /// Rust-native summary of current bulk/message pacing guidance.
    ///
    /// This is intended for embedding applications that need a typed signal for
    /// bitrate, batching, or quality adaptation rather than the lower-level
    /// output-parameter API.
    pub fn bulk_advice(&mut self) -> PragueRateAdvice {
        let (mut pacing_rate, mut packet_window, mut packet_burst, mut packet_size) = (0, 0, 0, 0);
        self.GetCCInfo(
            &mut pacing_rate,
            &mut packet_window,
            &mut packet_burst,
            &mut packet_size,
        );

        PragueRateAdvice::new(
            self.state,
            self.advice_assessment(),
            pacing_rate,
            packet_window,
            packet_burst,
            packet_size,
        )
    }

    /// Rust-native summary of current frame/video pacing guidance.
    pub fn video_advice(&mut self) -> PragueVideoRateAdvice {
        let (mut pacing_rate, mut frame_size, mut frame_window, mut packet_burst, mut packet_size) =
            (0, 0, 0, 0, 0);
        self.GetCCInfoVideo(
            &mut pacing_rate,
            &mut frame_size,
            &mut frame_window,
            &mut packet_burst,
            &mut packet_size,
        );

        PragueVideoRateAdvice {
            transport: PragueRateAdvice::new(
                self.state,
                self.advice_assessment(),
                pacing_rate,
                self.state.m_packet_window,
                packet_burst,
                packet_size,
            ),
            target_frame_size_bytes: frame_size,
            frame_window,
        }
    }

    /// Get ACK information to echo back (receiver side).
    pub fn GetACKInfo(
        &self,
        packets_received: &mut count_tp,
        packets_CE: &mut count_tp,
        packets_lost: &mut count_tp,
        error_L4S: &mut bool,
    ) {
        *packets_received = self.state.m_r_packets_received;
        *packets_CE = self.state.m_r_packets_CE;
        *packets_lost = self.state.m_r_packets_lost;
        *error_L4S = self.state.m_r_error_L4S;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_skips_zero_and_wraps() {
        let mut cc = PragueCC::default();
        let t1 = cc.Now();
        assert!(t1 > 0);
        // Ensure repeated calls are non-zero.
        for _ in 0..10 {
            assert_ne!(cc.Now(), 0);
        }
    }

    #[test]
    fn helpers_match_expected_simple_cases() {
        assert_eq!(mul_64_64_shift(2, 3, 0), 6);
        assert_eq!(mul_64_64_shift(u64::MAX, 2, 0), u64::MAX);
        assert_eq!(div_64_64_round(10, 3), 3); // 10 + 1 / 3 = 3
        assert_eq!(div_64_64_round(11, 3), 4); // 11 + 1 / 3 = 4
        assert_eq!(div_64_64_round(1, 0), u64::MAX);
    }

    #[test]
    fn bulk_advice_exposes_application_facing_guidance() {
        let mut cc = PragueCC::default();
        cc.state.m_alpha_ts = cc.Now();
        cc.state.m_vrtt = 0;
        cc.state.m_pacing_rate = 200_000;
        cc.state.m_packet_window = 11;
        cc.state.m_packet_burst = 3;
        cc.state.m_packet_size = 1200;
        cc.state.m_cc_state = cs_tp::cs_cong_avoid;
        cc.state.m_cca_mode = cca_tp::cca_prague_rate;
        cc.state.m_rtt = 11_000;
        cc.state.m_srtt = 12_000;
        cc.state.m_alpha = MAX_PROB / 8;

        let advice = cc.bulk_advice();

        assert_eq!(advice.pacing_rate_bytes_per_sec, 200_000 * 100 / 103);
        assert_eq!(advice.packet_window, 11);
        assert_eq!(advice.packet_burst, 3);
        assert_eq!(advice.packet_size_bytes, 1200);
        assert_eq!(advice.next_send_ecn, ecn_tp::ecn_l4s_id);
        assert_eq!(advice.controller_state, cs_tp::cs_cong_avoid);
        assert_eq!(advice.controller_mode, cca_tp::cca_prague_rate);
        assert_eq!(advice.last_rtt_us, 11_000);
        assert_eq!(advice.smoothed_rtt_us, 12_000);
        assert_eq!(advice.ce_ratio_ppm, 125_000);
        assert_eq!(advice.congestion_signal, PragueCongestionSignal::EcnMarked);
        assert!(!advice.l4s_fallback_active);
        assert_eq!(advice.pacing_rate_bits_per_sec(), (200_000 * 100 / 103) * 8);
    }

    #[test]
    fn video_advice_reports_frame_budget_guidance() {
        let mut cc = PragueCC::new(
            PRAGUE_INITMTU,
            60,
            10_000,
            PRAGUE_INITRATE,
            PRAGUE_INITWIN,
            PRAGUE_MINRATE,
            PRAGUE_MAXRATE,
        );
        cc.state.m_pacing_rate = 240_000;
        cc.state.m_packet_window = 8;
        cc.state.m_packet_burst = 2;
        cc.state.m_packet_size = 1200;

        let advice = cc.video_advice();

        assert_eq!(advice.transport.pacing_rate_bytes_per_sec, 240_000);
        assert_eq!(advice.transport.packet_burst, 2);
        assert_eq!(advice.transport.packet_size_bytes, 1200);
        assert_eq!(advice.target_frame_size_bytes, 2400);
        assert_eq!(advice.frame_window, 4);
    }

    #[test]
    fn bitrate_action_since_classifies_meaningful_changes() {
        let previous = PragueRateAdvice {
            pacing_rate_bytes_per_sec: 100_000,
            packet_window: 4,
            packet_burst: 1,
            packet_size_bytes: 1200,
            next_send_ecn: ecn_tp::ecn_l4s_id,
            controller_state: cs_tp::cs_cong_avoid,
            controller_mode: cca_tp::cca_prague_win,
            last_rtt_us: 10_000,
            smoothed_rtt_us: 10_000,
            virtual_rtt_us: 10_000,
            ce_ratio_ppm: 0,
            raw_alpha_ppm: 0,
            effective_alpha_ppm: 0,
            classic_ecn_alpha_floor_ppm: 0,
            classic_ecn_score: 0,
            classic_ecn_fallback_active: false,
            l4s_marking_error_active: false,
            congestion_signal: PragueCongestionSignal::Stable,
            l4s_fallback_active: false,
            packets_received: 0,
            packets_ce: 0,
            packets_lost: 0,
        };
        let mut current = previous;

        current.pacing_rate_bytes_per_sec = 108_000;
        assert_eq!(
            current.bitrate_action_since(&previous, 5),
            PragueBitrateAction::Increase
        );

        current.pacing_rate_bytes_per_sec = 97_000;
        assert_eq!(
            current.bitrate_action_since(&previous, 5),
            PragueBitrateAction::Hold
        );

        current.pacing_rate_bytes_per_sec = 90_000;
        assert_eq!(
            current.bitrate_action_since(&previous, 5),
            PragueBitrateAction::Decrease
        );
    }

    #[test]
    fn congestion_signal_prioritizes_fallback_then_loss_then_ecn() {
        let mut cc = PragueCC::default();
        cc.state.m_alpha_ts = cc.Now();
        cc.state.m_alpha = MAX_PROB / 16;
        assert_eq!(
            cc.bulk_advice().congestion_signal,
            PragueCongestionSignal::EcnMarked
        );

        cc.state.m_cc_state = cs_tp::cs_in_loss;
        assert_eq!(
            cc.bulk_advice().congestion_signal,
            PragueCongestionSignal::LossRecovery
        );

        cc.state.m_error_L4S = true;
        assert_eq!(
            cc.bulk_advice().congestion_signal,
            PragueCongestionSignal::L4sFallback
        );
    }

    #[test]
    fn ack_received_rejects_stale_counters() {
        let mut cc = PragueCC::default();
        cc.state.m_packets_received = 10;
        cc.state.m_packets_CE = 3;

        let mut inflight = 77;
        assert!(!cc.ACKReceived(9, 3, 0, 20, false, &mut inflight));
        assert_eq!(inflight, 77);

        assert!(!cc.ACKReceived(10, 2, 0, 20, false, &mut inflight));
        assert_eq!(inflight, 77);
        assert_eq!(cc.state.m_packets_received, 10);
        assert_eq!(cc.state.m_packets_CE, 3);
    }

    #[test]
    fn data_received_sequence_tracks_loss_marks_and_reordering() {
        let mut cc = PragueCC::default();

        cc.DataReceivedSequence(ecn_tp::ecn_l4s_id, 1);
        assert_eq!(cc.state.m_r_packets_received, 1);
        assert_eq!(cc.state.m_r_packets_lost, 0);
        assert_eq!(cc.state.m_r_packets_CE, 0);
        assert!(!cc.state.m_r_error_L4S);

        cc.DataReceivedSequence(ecn_tp::ecn_ce, 3);
        assert_eq!(cc.state.m_r_packets_received, 2);
        assert_eq!(cc.state.m_r_packets_lost, 1);
        assert_eq!(cc.state.m_r_packets_CE, 1);

        cc.DataReceivedSequence(ecn_tp::ecn_ect0, 2);
        assert_eq!(cc.state.m_r_packets_received, 3);
        assert_eq!(cc.state.m_r_packets_lost, 0);
        assert_eq!(cc.state.m_r_packets_CE, 1);
        assert!(cc.state.m_r_error_L4S);
    }

    #[test]
    fn ack_received_enters_loss_and_reordering_undo_restores_window() {
        let mut cc = PragueCC::default();
        cc.state.m_cc_state = cs_tp::cs_cong_avoid;
        cc.state.m_cca_mode = cca_tp::cca_prague_win;
        cc.state.m_srtt = 10_000;
        cc.state.m_vrtt = 10_000;
        cc.state.m_packet_size = 1200;
        cc.state.m_max_packet_size = 1200;
        cc.state.m_pacing_rate = 200_000;
        cc.state.m_fractional_window = 2_000_000_000;
        cc.state.m_packet_window = 10;
        cc.state.m_packets_received = 10;
        cc.state.m_packets_CE = 0;
        cc.state.m_packets_lost = 0;
        cc.state.m_packets_sent = 20;
        cc.state.m_alpha_ts = cc.Now().wrapping_sub(cc.state.m_vrtt);

        let original_window = cc.state.m_fractional_window;
        let mut inflight = 0;

        assert!(cc.ACKReceived(12, 0, 1, 30, false, &mut inflight));
        assert_eq!(cc.state.m_cc_state, cs_tp::cs_in_loss);
        assert_eq!(cc.state.m_lost_window, original_window / 2);
        assert_eq!(cc.state.m_fractional_window, original_window / 2);

        assert!(cc.ACKReceived(12, 0, 0, 31, false, &mut inflight));
        assert_eq!(cc.state.m_cc_state, cs_tp::cs_cong_avoid);
        assert_eq!(cc.state.m_lost_window, 0);
        assert_eq!(cc.state.m_fractional_window, original_window);
    }

    #[test]
    fn reset_cc_info_restores_initial_runtime_bounds() {
        let mut cc = PragueCC::default();
        cc.state.m_cc_state = cs_tp::cs_in_cwr;
        cc.state.m_cca_mode = cca_tp::cca_prague_rate;
        cc.state.m_pacing_rate = 333_333;
        cc.state.m_fractional_window = 999;
        cc.state.m_packet_burst = 7;
        cc.state.m_packet_size = 777;
        cc.state.m_packet_window = 99;
        cc.state.m_rtts_to_growth = 44;
        cc.state.m_lost_rtts_to_growth = 11;

        cc.ResetCCInfo();

        assert_eq!(cc.state.m_cc_state, cs_tp::cs_init);
        assert_eq!(cc.state.m_cca_mode, cca_tp::cca_prague_win);
        assert_eq!(cc.state.m_pacing_rate, cc.state.m_init_rate);
        assert_eq!(
            cc.state.m_fractional_window,
            cc.state.m_max_packet_size * 1_000_000
        );
        assert_eq!(cc.state.m_packet_burst, MIN_PKT_BURST);
        assert_eq!(cc.state.m_packet_size, cc.state.m_max_packet_size);
        assert_eq!(cc.state.m_packet_window, MIN_PKT_WIN);
        assert_eq!(cc.state.m_lost_rtts_to_growth, 0);
        assert_eq!(
            cc.state.m_rtts_to_growth,
            (cc.state.m_pacing_rate / RATE_STEP) as count_tp + (MIN_STEP as count_tp)
        );
    }

    #[test]
    fn set_max_packet_size_clamps_model_without_resetting_state() {
        let mut cc = PragueCC::default();
        cc.state.m_packet_size = 1500;
        cc.state.m_max_packet_size = 1500;
        cc.state.m_packet_window = 7;

        cc.set_max_packet_size(1200);

        assert_eq!(cc.state.m_max_packet_size, 1200);
        assert_eq!(cc.state.m_packet_size, 1200);
        assert_eq!(cc.state.m_packet_window, 7);

        cc.set_max_packet_size(0);
        assert_eq!(cc.state.m_max_packet_size, 1);
        assert_eq!(cc.state.m_packet_size, 1);
    }

    #[test]
    fn classic_aqm_floor_is_exposed_separately_from_raw_alpha() {
        let mut cc = PragueCC::default();
        for i in 0..20_000 {
            cc.observe_classic_aqm(ClassicAqmObservation {
                latest_rtt: std::time::Duration::from_millis(if i % 2 == 0 { 1 } else { 500 }),
                min_rtt: std::time::Duration::from_millis(1),
                ce_seen: true,
                ce_delta: 1,
                acked_delta: 10,
                congestion_avoidance_stable: true,
                ..Default::default()
            });
        }
        cc.state.m_alpha = 0;
        let advice = cc.bulk_advice();
        assert!(advice.raw_alpha_ppm <= advice.effective_alpha_ppm);
        assert!(advice.classic_ecn_score > 0);
        assert!(advice.classic_ecn_alpha_floor_ppm > 0);

        cc.set_classic_aqm_fallback_enabled(false);
        let disabled = cc.bulk_advice();
        assert_eq!(disabled.effective_alpha_ppm, disabled.raw_alpha_ppm);
        assert_eq!(disabled.classic_ecn_alpha_floor_ppm, 0);
    }
}
