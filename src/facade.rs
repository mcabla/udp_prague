//! Drop-in facade re-exporting the public API.
//!
//! The goal is to make switching between the C++ and Rust implementations as
//! frictionless as possible by keeping names and grouping similar.

pub use crate::congestion::*;
#[cfg(feature = "demo-app")]
pub use crate::core::AppError;
#[cfg(feature = "session")]
pub use crate::core::SessionError;
#[cfg(feature = "session")]
pub use crate::core::{
    PragueAckCounters, PragueAckFeedback, PragueAckReport, PragueBulkTransferReport,
    PragueFrameWindowMetrics, PraguePacketWindowMetrics, PragueQueuedVideoFrame,
    PragueReceivedBulkPacket, PragueReceivedBulkPacketView, PragueReceivedFramePacket,
    PragueReceivedFramePacketView, PragueReceivedPacket, PragueReceivedPacketAndAck,
    PragueReceivedPacketAndAckView, PragueReceivedPacketView, PragueReceivedSegment,
    PragueReceivedVideoFrame, PragueReceiverReassemblyLimits, PragueReceiverSession,
    PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent,
    PragueSegmentReceiverSession, PragueSegmentSendReport, PragueSegmentSenderSession,
    PragueSendAckEvent, PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendReport,
    PragueSendRfc8888AckEvent, PragueSenderSession, PragueSessionConfig, PragueVideoAckFeedback,
    PragueVideoReceiverSession, PragueVideoSendReport, PragueVideoSenderSession,
    PragueVideoSessionConfig, Reporter, RunnerConfig, FRAME_DURATION, FRAME_PER_SECOND, PORT,
    RFC8888_ACKPERIOD,
};
#[cfg(not(feature = "session"))]
pub use crate::core::{
    PragueAckCounters, PragueFrameWindowMetrics, PraguePacketWindowMetrics, PragueRecvAckEvent,
    PragueRecvDataEvent, PragueRecvRfc8888AckEvent, PragueSendAckEvent, PragueSendDataEvent,
    PragueSendFrameDataEvent, PragueSendRfc8888AckEvent, Reporter, RunnerConfig, FRAME_DURATION,
    FRAME_PER_SECOND, MAX_TIMEOUT, PORT, RFC8888_ACKPERIOD,
};
pub use crate::core::{RunnerError, UdpSocketError};
#[cfg(feature = "demo-app")]
pub use crate::demo::json_writer;
#[cfg(feature = "demo-app")]
pub use crate::demo::{AppStuff, REPT_PERIOD};
pub use crate::net::{Endpoint, SocketPlatformSupport, UDPSocket};
pub use crate::protocol::*;
