// SPDX-License-Identifier: GPL-2.0-only
// SPDX-FileCopyrightText: 2024 Nokia
//
// Adapted for the Rust UDP Prague userspace controller from the Linux
// TCP-Prague Classic-ECN/AQM detector. See NOTICE and LICENSES/GPL-2.0-only.txt.

//! RFC 9331-compatible passive Classic-ECN/AQM monitor for Prague.
//!
//! The detector is intentionally kept in the canonical `udp_prague` crate.  It
//! follows the fixed-point equations in the L4STeam Linux Prague reference
//! (`testing`, revision `c6c391a4c5b78a1bff5954e7c25406b4964f50f0`), while
//! accepting QUIC recovery samples instead of TCP's rate-sample structure.
//!
//! Source provenance (reviewed 2026-08-31):
//! * safety requirements: <https://www.rfc-editor.org/rfc/rfc9331.html>
//! * pinned reference implementation:
//!   <https://github.com/L4STeam/linux/blob/c6c391a4c5b78a1bff5954e7c25406b4964f50f0/net/ipv4/tcp_prague.c>
//! * current reference branch: <https://github.com/L4STeam/linux/blob/testing/net/ipv4/tcp_prague.c>
//! * current kernel comparison supplied for this port:
//!   <https://github.com/minuscat/l4steam-6.18.y/compare/linux-6.18.y...testing-net-next39>
//! * the relevant Linux routines are `prague_classic_ecn_detection` and
//!   `prague_classic_ecn_fallback`.  The latter clamps alpha for safety; it
//!   does not rewrite an L4S packet's ECT(1) codepoint to ECT(0).
//!
//! This Rust monitor intentionally keeps that separation: Classic-AQM
//! compatibility changes the congestion response, while ECN marking remains
//! ECT(1) unless an independent marking-integrity failure disables ECN.
//! The monitor never changes ECN marking by itself; the Prague controller uses
//! its assessment to apply the RFC 9331 safety response.

use std::time::Duration;

const ALPHA_BITS: u32 = 24;
const MAX_ALPHA: u64 = 1 << ALPHA_BITS;
// Upper bounds of the RTT estimator's gain shifts (`PRAGUE_MAX_SRTT_BITS`,
// `PRAGUE_MAX_MDEV_BITS`); `update_rtt_scaling` lowers them with the window.
const SRTT_SHIFT: u32 = 18;
const MDEV_SHIFT: u32 = 19;
const INIT_ADJ_US: u64 = 1 << 18;
const INIT_MDEV_CARRY: u64 = 741_455;
const INIT_DEPTH_CARRY: u64 = 741_455 >> 1;
const V: u32 = 1;
const D: u32 = 1;
const L_STICKY: u64 = 16 << (ALPHA_BITS - V);
const CLASSIC_ECN: u64 = L_STICKY + MAX_ALPHA;
const C_STICKY: u64 = CLASSIC_ECN + L_STICKY;
// log2(750us) and log2(2ms), upscaled by the Linux detector's weights.
const V0_LG: u64 = 160_234_941 >> V;
const D0_LG: u64 = 183_975_331 >> D;

/// State of the passive Classic-AQM detector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassicAqmState {
    /// No CE/RTT evidence is available yet.
    #[default]
    InsufficientEvidence,
    /// CE has been observed but the path still looks L4S-like.
    L4sLikely,
    /// The detector is in the transition region.
    ClassicSuspected,
    /// The path is sufficiently Classic-like for the RFC 9331 response.
    ClassicCompatible,
}

/// Inputs consumed by one detector update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassicAqmObservation {
    pub latest_rtt: Duration,
    pub min_rtt: Duration,
    pub ce_seen: bool,
    pub ce_delta: u64,
    pub acked_delta: u64,
    pub app_limited: bool,
    /// True when the sample is from congestion avoidance rather than startup,
    /// recovery, or an otherwise unstable epoch.
    pub congestion_avoidance_stable: bool,
    /// Optional transport-neutral eligibility override. `None` preserves the
    /// historical derivation (`!app_limited && congestion_avoidance_stable`).
    /// Ineligible samples are not allowed to update the detector's slow RTT /
    /// MDEV state, preventing a burst/idle media cadence from masquerading as
    /// a Classic-AQM sawtooth.
    pub detector_eligible: Option<bool>,
}

impl ClassicAqmObservation {
    /// Whether this sample may update the detector.
    pub fn is_detector_eligible(&self) -> bool {
        self.detector_eligible
            .unwrap_or(!self.app_limited && self.congestion_avoidance_stable)
    }
}

/// Cheap, serializable result of the monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClassicAqmAssessment {
    pub state: ClassicAqmState,
    /// Linux `classic_ecn` score, scaled by `1 << 24`.
    pub classic_ecn_score: u64,
    /// Minimum effective Prague alpha, scaled by `1 << 20` (Prague's scale).
    pub alpha_floor: u64,
}

/// Stateful passive detector.  It is `Copy` so a Quinn path can preserve it
/// across NAT rebinding without sharing state with another path/connection.
#[derive(Clone, Copy, Debug)]
pub struct ClassicAqmMonitor {
    initialized: bool,
    saw_ce: bool,
    stable_samples: u32,
    srtt_shift: u32,
    mdev_shift: u32,
    srtt_pace_us: u64,
    mdev_pace_us: u64,
    rest_mdev: u64,
    rest_depth: u64,
    score: u64,
    state: ClassicAqmState,
}

impl Default for ClassicAqmMonitor {
    fn default() -> Self {
        Self {
            initialized: false,
            saw_ce: false,
            stable_samples: 0,
            srtt_shift: SRTT_SHIFT,
            mdev_shift: MDEV_SHIFT,
            srtt_pace_us: 0,
            mdev_pace_us: 0,
            rest_mdev: INIT_MDEV_CARRY,
            rest_depth: INIT_DEPTH_CARRY,
            score: 0,
            state: ClassicAqmState::InsufficientEvidence,
        }
    }
}

#[inline]
fn micros(value: Duration) -> u64 {
    value.as_micros().min(u128::from(u64::MAX)) as u64
}

#[inline]
fn ilog2(value: u64) -> u32 {
    if value == 0 {
        0
    } else {
        63 - value.leading_zeros()
    }
}

impl ClassicAqmMonitor {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Observe one validated recovery extent and score it, as tcp_prague.c
    /// does once per round.  CE is deliberately a gate: RTT variation without
    /// CE cannot prove a Classic ECN AQM is present.
    pub fn observe(&mut self, observation: ClassicAqmObservation) -> ClassicAqmAssessment {
        self.observe_sample(observation, true)
    }

    /// Feed one RTT sample to the slow RTT/MDEV estimator without scoring it
    /// (tcp_prague.c's per-ACK `prague_rtt_estimator`).  Callers that see
    /// every ACK score once per round with [`Self::observe`] and pass the
    /// other samples here.
    pub fn observe_rtt(&mut self, observation: ClassicAqmObservation) -> ClassicAqmAssessment {
        self.observe_sample(observation, false)
    }

    /// Scale the estimator's gain to the window (in packets) after a window
    /// reduction, as `prague_update_rtt_scaling` does with ssthresh: a gain of
    /// 1/2^18 per sample would keep an early outlier in the baseline for
    /// minutes at a few thousand samples per second.
    pub fn update_rtt_scaling(&mut self, window_packets: u64) {
        let lg = ilog2(window_packets.max(1));
        let shift = (lg + (lg >> 1) + 1).min(SRTT_SHIFT);
        let old = self.srtt_shift;
        self.srtt_shift = shift;
        self.mdev_shift = shift + 1;
        if shift > old {
            let d = shift - old;
            self.srtt_pace_us = self.srtt_pace_us.saturating_mul(1 << d);
            self.mdev_pace_us = self.mdev_pace_us.saturating_mul(1 << d);
            self.rest_depth = self.rest_depth.saturating_mul(1 << d);
            self.rest_mdev = self.rest_mdev.saturating_mul(1 << d);
        } else if shift < old {
            let d = old - shift;
            self.srtt_pace_us >>= d;
            self.mdev_pace_us >>= d;
            self.rest_depth = (self.rest_depth >> d).max(1);
            self.rest_mdev = (self.rest_mdev >> d).max(1);
        }
    }

    fn observe_sample(
        &mut self,
        observation: ClassicAqmObservation,
        score_round: bool,
    ) -> ClassicAqmAssessment {
        let latest = micros(observation.latest_rtt);
        if latest == 0 {
            return self.assessment();
        }
        self.saw_ce |= observation.ce_seen && observation.ce_delta > 0;
        if !observation.is_detector_eligible() {
            // Start a fresh eligible warm-up after an application-limited or
            // unstable epoch. The independent path baseline remains owned by
            // Quinn; this detector does not manufacture capacity-seeking load.
            if score_round {
                self.stable_samples = 0;
            }
            if self.saw_ce {
                self.state = if self.score == 0 {
                    ClassicAqmState::L4sLikely
                } else {
                    self.state_for_score()
                };
            }
            return self.assessment();
        }
        let min_rtt = micros(observation.min_rtt);
        if !self.initialized {
            // Seed at the path's minimum, like tcp_prague.c seeds at the
            // handshake RTT: the first eligible sample can be a burst's.
            let seed = if min_rtt > 0 && min_rtt < latest {
                min_rtt
            } else {
                latest
            };
            self.initialized = true;
            self.srtt_pace_us = seed.saturating_mul(1u64 << self.srtt_shift);
            self.mdev_pace_us = 1 << self.mdev_shift;
        } else {
            let srtt = self.srtt_pace_us >> self.srtt_shift;
            let error = latest as i128 - srtt as i128;
            self.srtt_pace_us = if error >= 0 {
                self.srtt_pace_us.saturating_add(error as u64)
            } else {
                self.srtt_pace_us.saturating_sub((-error) as u64)
            };
            let delta = srtt.abs_diff(latest);
            let mdev = self.mdev_pace_us >> self.mdev_shift;
            let error = delta as i128 - mdev as i128;
            self.mdev_pace_us = if error >= 0 {
                self.mdev_pace_us.saturating_add(error as u64)
            } else {
                self.mdev_pace_us.saturating_sub((-error) as u64)
            };
        }

        if !self.saw_ce {
            self.state = ClassicAqmState::InsufficientEvidence;
            return self.assessment();
        }
        if !score_round {
            if self.state == ClassicAqmState::InsufficientEvidence {
                self.state = self.state_for_score();
            }
            return self.assessment();
        }
        // A CE extent without newly acknowledged packets is not a usable
        // congestion sample.  Keep the detector's RTT state warm, but do not
        // let malformed/empty feedback advance the Classic-AQM score.
        if observation.acked_delta == 0 {
            self.state = self.state_for_score();
            return self.assessment();
        }
        self.stable_samples = self.stable_samples.saturating_add(1);
        if self.stable_samples < 2 {
            self.state = ClassicAqmState::L4sLikely;
            return self.assessment();
        }

        // Keep the same fixed-point shape as tcp_prague.c: the geometric
        // residual is carried between rounds and the logarithm is integer-only.
        let adj = INIT_ADJ_US >> (MDEV_SHIFT - self.mdev_shift);
        let mdev = (self.mdev_pace_us >> self.mdev_shift)
            .saturating_mul(self.rest_mdev)
            .saturating_add(adj);
        let mdev_lg = ilog2(mdev).max(self.mdev_shift) - self.mdev_shift;
        self.rest_mdev = (mdev >> mdev_lg).max(1);
        let mut score = self.score as i128;
        score += ((u64::from(mdev_lg)) << (ALPHA_BITS - V)) as i128 - V0_LG as i128;

        let srtt = self.srtt_pace_us >> self.srtt_shift;
        if min_rtt > 0 && srtt > min_rtt {
            let depth = (srtt - min_rtt)
                .saturating_mul(self.rest_depth)
                .saturating_add(adj / 2);
            let depth_lg = ilog2(depth).max(self.srtt_shift) - self.srtt_shift;
            self.rest_depth = (depth >> depth_lg).max(1);
            let weighted = (u64::from(depth_lg)) << (ALPHA_BITS - D);
            if weighted > D0_LG {
                score += (weighted - D0_LG) as i128;
            }
        }
        self.score = score.clamp(0, C_STICKY as i128) as u64;
        self.state = self.state_for_score();
        self.assessment()
    }

    fn state_for_score(&self) -> ClassicAqmState {
        if !self.saw_ce {
            ClassicAqmState::InsufficientEvidence
        } else if self.score < L_STICKY {
            ClassicAqmState::L4sLikely
        } else if self.score < CLASSIC_ECN {
            ClassicAqmState::ClassicSuspected
        } else {
            ClassicAqmState::ClassicCompatible
        }
    }

    pub fn assessment(&self) -> ClassicAqmAssessment {
        let score = self.score.min(CLASSIC_ECN);
        let alpha_floor_24 = if score > L_STICKY {
            ((score - L_STICKY) >> 1).saturating_add((score - L_STICKY) >> 3)
        } else {
            0
        };
        ClassicAqmAssessment {
            state: self.state,
            classic_ecn_score: self.score,
            alpha_floor: ((alpha_floor_24.saturating_mul(1 << 20)) / MAX_ALPHA).min(1 << 20),
        }
    }

    pub fn state(&self) -> ClassicAqmState {
        self.state
    }

    pub fn classic_ecn_score(&self) -> u64 {
        self.score
    }

    pub fn alpha_floor(&self) -> u64 {
        self.assessment().alpha_floor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rtt_ms: u64, min_ms: u64) -> ClassicAqmObservation {
        ClassicAqmObservation {
            latest_rtt: Duration::from_millis(rtt_ms),
            min_rtt: Duration::from_millis(min_ms),
            ce_seen: true,
            ce_delta: 1,
            acked_delta: 10,
            congestion_avoidance_stable: true,
            ..Default::default()
        }
    }

    #[test]
    fn ce_gate_rejects_rtt_only_evidence() {
        let mut monitor = ClassicAqmMonitor::default();
        let mut observation = sample(40, 10);
        observation.ce_seen = false;
        observation.ce_delta = 0;
        for _ in 0..20 {
            let assessment = monitor.observe(observation);
            assert_eq!(assessment.state, ClassicAqmState::InsufficientEvidence);
            assert_eq!(assessment.classic_ecn_score, 0);
        }
    }

    #[test]
    fn app_limited_samples_do_not_advance_detector() {
        let mut monitor = ClassicAqmMonitor::default();
        let mut observation = sample(100, 10);
        observation.app_limited = true;
        for _ in 0..20 {
            monitor.observe(observation);
        }
        assert_eq!(monitor.classic_ecn_score(), 0);
        assert_eq!(monitor.state(), ClassicAqmState::L4sLikely);
    }

    #[test]
    fn ineligible_burst_rtt_does_not_poison_slow_smoothing() {
        let mut monitor = ClassicAqmMonitor::default();
        let eligible = sample(10, 10);
        monitor.observe(eligible);
        let baseline = monitor.srtt_pace_us;

        // A large, CE-marked application burst is explicitly ineligible. It
        // must not pull the detector's slow RTT/MDEV state toward 500 ms.
        for _ in 0..100 {
            monitor.observe(ClassicAqmObservation {
                latest_rtt: Duration::from_millis(500),
                min_rtt: Duration::from_millis(10),
                ce_seen: true,
                ce_delta: 1,
                acked_delta: 100,
                detector_eligible: Some(false),
                ..Default::default()
            });
        }
        assert_eq!(monitor.srtt_pace_us, baseline);
        assert_eq!(monitor.mdev_pace_us, 1 << MDEV_SHIFT);

        // Eligibility resumes with a clean warm-up, rather than inheriting
        // the application's burst cadence as Classic-AQM evidence.
        monitor.observe(eligible);
        assert_eq!(monitor.stable_samples, 1);
    }

    #[test]
    fn empty_ack_extent_does_not_advance_detector() {
        let mut monitor = ClassicAqmMonitor::default();
        let mut observation = sample(500, 1);
        observation.acked_delta = 0;
        for _ in 0..20_000 {
            monitor.observe(observation);
        }
        assert_eq!(monitor.classic_ecn_score(), 0);
        assert_eq!(monitor.alpha_floor(), 0);
    }

    #[test]
    fn arithmetic_is_bounded_for_extreme_rtt() {
        let mut monitor = ClassicAqmMonitor::default();
        let observation = ClassicAqmObservation {
            latest_rtt: Duration::from_secs(60 * 60),
            min_rtt: Duration::from_nanos(1),
            ce_seen: true,
            ce_delta: u64::MAX,
            acked_delta: u64::MAX,
            congestion_avoidance_stable: true,
            ..Default::default()
        };
        for _ in 0..100 {
            monitor.observe(observation);
        }
        assert!(monitor.classic_ecn_score() <= C_STICKY);
        assert!(monitor.alpha_floor() <= (1 << 20));
    }

    #[test]
    fn arithmetic_is_bounded_for_maximum_duration() {
        let mut monitor = ClassicAqmMonitor::default();
        let observation = ClassicAqmObservation {
            latest_rtt: Duration::from_micros(u64::MAX),
            min_rtt: Duration::from_micros(u64::MAX),
            ce_seen: true,
            ce_delta: 1,
            acked_delta: 1,
            congestion_avoidance_stable: true,
            ..Default::default()
        };
        for _ in 0..16 {
            monitor.observe(observation);
        }
        assert!(monitor.classic_ecn_score() <= C_STICKY);
        assert!(monitor.alpha_floor() <= (1 << 20));
    }

    #[test]
    fn sustained_queue_depth_enters_classic_region() {
        let mut monitor = ClassicAqmMonitor::default();
        for i in 0..20_000 {
            let rtt = if i % 2 == 0 { 1 } else { 500 };
            monitor.observe(sample(rtt, 1));
        }
        assert!(matches!(
            monitor.state(),
            ClassicAqmState::ClassicSuspected | ClassicAqmState::ClassicCompatible
        ));
        assert!(monitor.alpha_floor() > 0);
    }

    #[test]
    fn stable_high_base_rtt_is_not_classic_by_itself() {
        let mut monitor = ClassicAqmMonitor::default();
        // A 500 ms propagation path exceeds a 30-fps frame interval, but has
        // no queue-depth sawtooth. CE is present so the test exercises the
        // detector's actual RTT/depth evidence gate rather than the no-CE
        // fast path.
        for _ in 0..20_000 {
            monitor.observe(sample(500, 500));
        }
        assert!(!matches!(
            monitor.state(),
            ClassicAqmState::ClassicSuspected | ClassicAqmState::ClassicCompatible
        ));
        assert_eq!(monitor.alpha_floor(), 0);
    }

    #[test]
    fn reference_vector_exposes_intermediate_score_and_floor() {
        let mut monitor = ClassicAqmMonitor::default();
        let mut checkpoints = Vec::new();
        for i in 0..20_000 {
            let assessment = monitor.observe(sample(if i % 2 == 0 { 1 } else { 500 }, 1));
            if matches!(i, 0 | 1 | 7 | 31 | 127 | 1_023 | 4_095 | 19_999) {
                checkpoints.push((i, assessment.classic_ecn_score, assessment.alpha_floor));
            }
        }
        // These checkpoints pin the fixed-point implementation to the
        // reference detector's bounded convergence.  In particular, no
        // floor is emitted before enough stable CE evidence has accumulated,
        // while a sustained 500 ms queue reaches the sticky classic region.
        assert_eq!(
            checkpoints,
            vec![
                (0, 0, 0),
                (1, 0, 0),
                (7, 0, 0),
                (31, 0, 0),
                (127, 0, 0),
                (1_023, 0, 0),
                (4_095, 285_212_672, 655_360),
                (19_999, 285_212_672, 655_360),
            ]
        );
    }

    #[test]
    fn empty_monitor_starts_with_insufficient_evidence() {
        let monitor = ClassicAqmMonitor::default();
        let assessment = monitor.assessment();
        assert_eq!(assessment.state, ClassicAqmState::InsufficientEvidence);
        assert_eq!(assessment.classic_ecn_score, 0);
        assert_eq!(assessment.alpha_floor, 0);
    }

    #[test]
    fn shallow_stable_queue_does_not_force_immediate_fallback() {
        let mut monitor = ClassicAqmMonitor::default();
        for _ in 0..1_000 {
            monitor.observe(sample(20, 19));
        }
        assert_ne!(monitor.state(), ClassicAqmState::ClassicCompatible);
    }

    #[test]
    fn fallback_clears_only_after_score_decays() {
        let mut monitor = ClassicAqmMonitor::default();
        for i in 0..20_000 {
            monitor.observe(sample(if i % 2 == 0 { 1 } else { 500 }, 1));
        }
        assert_eq!(monitor.state(), ClassicAqmState::ClassicCompatible);

        // Hysteresis: one clean sample must not immediately clear a sticky
        // Classic-compatible classification.
        monitor.observe(sample(1, 1));
        assert_eq!(monitor.state(), ClassicAqmState::ClassicCompatible);

        // Sustained low-queue evidence eventually drives the score back into
        // the L4S-like region, allowing normal Prague behaviour to resume.
        for _ in 0..12_000_000 {
            monitor.observe(sample(1, 1));
        }
        assert_ne!(monitor.state(), ClassicAqmState::ClassicCompatible);
        assert_eq!(monitor.alpha_floor(), 0);
    }

    #[test]
    fn reset_returns_monitor_to_fresh_state() {
        let mut monitor = ClassicAqmMonitor::default();
        for i in 0..20_000 {
            monitor.observe(sample(if i % 2 == 0 { 1 } else { 500 }, 1));
        }
        assert!(monitor.classic_ecn_score() > 0);
        monitor.reset();
        assert_eq!(monitor.assessment(), ClassicAqmAssessment::default());
    }

    /// Deterministic stand-in for RTT samples on an L4S DualPI2 path: mostly
    /// within 1 ms of the minimum, one in twenty 1-3 ms above it.
    fn l4s_excess_us(i: u64) -> u64 {
        let r = i
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
            >> 33;
        if r.is_multiple_of(20) {
            1_000 + r % 2_000
        } else {
            r % 1_000
        }
    }

    /// Feeds `acks` samples the way a per-ACK caller does: every sample
    /// updates the estimator, one per `window` is scored, and the gain is
    /// scaled to the window after the first reduction.  Returns the first ACK
    /// in `ClassicCompatible` and how many ACKs were spent there.
    fn per_round(
        monitor: &mut ClassicAqmMonitor,
        acks: u64,
        window: u64,
        excess_us: impl Fn(u64) -> u64,
    ) -> (Option<u64>, u64) {
        let (mut first, mut inside) = (None, 0);
        for i in 0..acks {
            if i == 300 {
                monitor.update_rtt_scaling(window);
            }
            let mut observation = sample(0, 0);
            observation.min_rtt = Duration::from_micros(20_000);
            observation.latest_rtt = Duration::from_micros(20_000 + excess_us(i));
            observation.ce_delta = u64::from(i.is_multiple_of(50));
            observation.ce_seen = observation.ce_delta > 0;
            let state = if i.is_multiple_of(window) {
                monitor.observe(observation).state
            } else {
                monitor.observe_rtt(observation).state
            };
            if state == ClassicAqmState::ClassicCompatible {
                first.get_or_insert(i);
                inside += 1;
            }
        }
        (first, inside)
    }

    #[test]
    fn high_first_sample_does_not_pin_classic_fallback() {
        // The first eligible sample came from a burst 10 ms above the floor.
        // Seeded from it at a 1/2^18 gain, per-ACK scoring reached the
        // sticky classic region after ~10k ACKs and stayed there on a flat
        // L4S path.
        let mut monitor = ClassicAqmMonitor::default();
        let mut burst = sample(30, 20);
        burst.ce_seen = false;
        burst.ce_delta = 0;
        monitor.observe_rtt(burst);
        let (first, _) = per_round(&mut monitor, 80_000, 115, l4s_excess_us);
        assert_eq!(first, None);
        assert_eq!(monitor.alpha_floor(), 0);
    }

    #[test]
    fn classic_sawtooth_is_detected_within_seconds() {
        // A single-queue Classic ECN AQM: the queue ramps 1 -> 8 ms and drains.
        let mut monitor = ClassicAqmMonitor::default();
        let (first, inside) = per_round(&mut monitor, 80_000, 115, |i| {
            1_000 + 7_000 * (i % 800) / 800
        });
        assert!(first.is_some_and(|ack| ack < 6_500), "{first:?}");
        assert!(inside > 72_000, "{inside}");
    }

    #[test]
    fn rtt_scaling_keeps_the_estimates() {
        let mut monitor = ClassicAqmMonitor::default();
        for i in 0..1_000 {
            monitor.observe_rtt(sample(20 + i % 3, 20));
        }
        let (srtt, mdev) = (
            monitor.srtt_pace_us >> monitor.srtt_shift,
            monitor.mdev_pace_us >> monitor.mdev_shift,
        );
        monitor.update_rtt_scaling(115);
        assert_eq!((monitor.srtt_shift, monitor.mdev_shift), (10, 11));
        assert_eq!(monitor.srtt_pace_us >> monitor.srtt_shift, srtt);
        assert_eq!(monitor.mdev_pace_us >> monitor.mdev_shift, mdev);
        monitor.update_rtt_scaling(u64::MAX);
        assert_eq!(
            (monitor.srtt_shift, monitor.mdev_shift),
            (SRTT_SHIFT, MDEV_SHIFT)
        );
        assert_eq!(monitor.srtt_pace_us >> monitor.srtt_shift, srtt);
    }

    #[test]
    fn zero_and_small_rtt_values_are_safe() {
        let mut monitor = ClassicAqmMonitor::default();
        for rtt in [
            Duration::ZERO,
            Duration::from_nanos(1),
            Duration::from_micros(1),
        ] {
            monitor.observe(ClassicAqmObservation {
                latest_rtt: rtt,
                min_rtt: Duration::from_nanos(1),
                ce_seen: true,
                ce_delta: 1,
                acked_delta: 1,
                congestion_avoidance_stable: true,
                ..Default::default()
            });
        }
        assert!(monitor.classic_ecn_score() <= C_STICKY);
    }
}
