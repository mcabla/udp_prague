//! UDP Prague (L4S) example project ported to Rust.
//!
//! This crate aims to be a near-literal port of the reference C++ code, keeping
//! file/module structure and function naming close enough that it is easy to
//! compare the two implementations line-by-line.
//!
//! The hot path is the congestion controller (`PragueCC`) and related packet
//! parsing. Those modules avoid heap allocations and preserve the original
//! wrap-around arithmetic semantics.
//!
//! The reusable protocol/runtime surface is always available.
//! The higher-level session wrappers are behind the `session` feature.
//! The demo application's CLI/reporting adapter (`app_stuff`) is behind the
//! `demo-app` feature.
//!
//! Module layout:
//! - `udp_prague::core`: runner loops, config, reporting, and errors.
//! - `udp_prague::congestion`: Prague CC algorithm/types.
//! - `udp_prague::protocol`: packet formats and wire helpers.
//! - `udp_prague::net`: UDP socket backend.
//! - `udp_prague::core` session wrappers: available with the `session` feature.
//! - `udp_prague::demo`: reference-style CLI/reporting layer (`demo-app` only).

#![forbid(unsafe_op_in_unsafe_fn)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::identity_op)]

pub mod congestion;
pub mod core;
#[cfg(feature = "demo-app")]
pub mod demo;
pub mod net;
pub mod protocol;

/// A "drop-in" compatibility facade that re-exports the primary types.
pub mod facade;

// Backward-compatible top-level module aliases for existing code.
pub use crate::congestion::prague_cc;
pub use crate::core::{error, runner, runtime};
#[cfg(feature = "demo-app")]
pub use crate::demo::app as app_stuff;
#[cfg(feature = "demo-app")]
pub use crate::demo::json_writer;
pub use crate::net::udpsocket;
pub use crate::protocol::pkt_format;

// Commonly used exports (mirrors typical C++ includes).
pub use crate::congestion::{
    cca_tp, count_tp, cs_tp, ecn_tp, fps_tp, prob_tp, rate_tp, size_tp, time_tp, window_tp,
    PragueBitrateAction, PragueCC, PragueCongestionSignal, PragueRateAdvice, PragueState,
    PragueVideoRateAdvice, PRAGUE_INITMTU, PRAGUE_MAXRATE, PRAGUE_MINRATE,
};
pub use crate::congestion::{
    ClassicAqmAssessment, ClassicAqmMonitor, ClassicAqmObservation, ClassicAqmState,
};
#[cfg(feature = "demo-app")]
pub use crate::core::AppError;
#[cfg(feature = "session")]
pub use crate::core::SessionError;
#[cfg(feature = "session")]
pub use crate::core::{
    PragueAckCounters, PragueAckFeedback, PragueAckReport, PragueBulkTransferReport,
    PragueClassicAqmEvent, PragueFrameWindowMetrics, PraguePacketWindowMetrics,
    PragueQueuedVideoFrame, PragueReceivedBulkPacket, PragueReceivedBulkPacketView,
    PragueReceivedFramePacket, PragueReceivedFramePacketView, PragueReceivedPacket,
    PragueReceivedPacketAndAck, PragueReceivedPacketAndAckView, PragueReceivedPacketView,
    PragueReceivedSegment, PragueReceivedVideoFrame, PragueReceiverReassemblyLimits,
    PragueReceiverSession, PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent,
    PragueSegmentReceiverSession, PragueSegmentSendReport, PragueSegmentSenderSession,
    PragueSendAckEvent, PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendReport,
    PragueSendRfc8888AckEvent, PragueSenderSession, PragueSessionConfig, PragueVideoAckFeedback,
    PragueVideoReceiverSession, PragueVideoSendReport, PragueVideoSenderSession,
    PragueVideoSessionConfig, Reporter, RunnerConfig, FRAME_DURATION, FRAME_PER_SECOND, PORT,
    RFC8888_ACKPERIOD,
};
#[cfg(not(feature = "session"))]
pub use crate::core::{
    PragueAckCounters, PragueClassicAqmEvent, PragueFrameWindowMetrics, PraguePacketWindowMetrics,
    PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent, PragueSendAckEvent,
    PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendRfc8888AckEvent, Reporter,
    RunnerConfig, FRAME_DURATION, FRAME_PER_SECOND, MAX_TIMEOUT, PORT, RFC8888_ACKPERIOD,
};
pub use crate::core::{RunnerError, UdpSocketError};
pub use crate::net::{Endpoint, SocketPlatformSupport, UDPSocket};
