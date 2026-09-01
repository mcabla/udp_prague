//! Reusable library runtime surface.
//!
//! This module groups the core types needed when embedding UDP Prague as a
//! library: errors, runtime configuration, reporting hooks, and the sender /
//! receiver loops.

pub mod error;
pub mod runner;
pub mod runtime;
#[cfg(feature = "session")]
pub mod session;

#[cfg(feature = "demo-app")]
pub use self::error::AppError;
#[cfg(feature = "session")]
pub use self::error::SessionError;
pub use self::error::{RunnerError, UdpSocketError};
#[cfg(feature = "demo-app")]
pub use self::runner::{run_receiver, run_sender};
pub use self::runner::{run_receiver_with_reporter, run_sender_with_reporter, MAX_TIMEOUT};
pub use self::runtime::{
    PragueAckCounters, PragueClassicAqmEvent, PragueFrameWindowMetrics, PraguePacketWindowMetrics,
    PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent, PragueSendAckEvent,
    PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendRfc8888AckEvent, Reporter,
    RunnerConfig, FRAME_DURATION, FRAME_PER_SECOND, PORT, RFC8888_ACKPERIOD,
};
#[cfg(feature = "session")]
pub use self::session::{
    PragueAckFeedback, PragueAckReport, PragueBulkTransferReport, PragueQueuedVideoFrame,
    PragueReceivedBulkPacket, PragueReceivedBulkPacketView, PragueReceivedFramePacket,
    PragueReceivedFramePacketView, PragueReceivedPacket, PragueReceivedPacketAndAck,
    PragueReceivedPacketAndAckView, PragueReceivedPacketView, PragueReceivedSegment,
    PragueReceivedVideoFrame, PragueReceiverReassemblyLimits, PragueReceiverSession,
    PragueSegmentReceiverSession, PragueSegmentSendReport, PragueSegmentSenderSession,
    PragueSendReport, PragueSenderSession, PragueSessionConfig, PragueVideoAckFeedback,
    PragueVideoReceiverSession, PragueVideoSendReport, PragueVideoSenderSession,
    PragueVideoSessionConfig,
};
