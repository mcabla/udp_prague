//! Error types used across the UDP Prague Rust port.

use core::fmt;
#[cfg(feature = "demo-app")]
use std::io;

#[cfg(feature = "session")]
use crate::congestion::{count_tp, time_tp};
use crate::protocol::pkt_format::PacketError;

/// Errors that can occur while using [`crate::net::UDPSocket`].
#[derive(Debug)]
pub enum UdpSocketError {
    /// Operation is not supported on the current platform.
    NotSupported(&'static str),
    /// A system call failed.
    Syscall {
        /// Name of the failed syscall.
        call: &'static str,
        /// OS error code (e.g., `errno` on Unix).
        code: i32,
    },
    /// Invalid input provided by the caller.
    InvalidInput(&'static str),
    /// Address string could not be parsed as IPv4/IPv6.
    UnsupportedAddress,
    /// Received an unexpected control message.
    ///
    /// The reference implementation treats this as fatal; we surface it as an error.
    UnexpectedControlMessage { level: i32, ty: i32 },
}

impl fmt::Display for UdpSocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UdpSocketError::NotSupported(what) => write!(f, "not supported: {what}"),
            UdpSocketError::Syscall { call, code } => {
                write!(f, "{call} failed (os error {code})")
            }
            UdpSocketError::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            UdpSocketError::UnsupportedAddress => write!(f, "unsupported address type"),
            UdpSocketError::UnexpectedControlMessage { level, ty } => {
                write!(f, "unexpected control message (level={level}, type={ty})")
            }
        }
    }
}

impl std::error::Error for UdpSocketError {}

/// Errors that can occur while configuring the sender/receiver app state.
#[cfg(feature = "demo-app")]
#[derive(Debug)]
pub enum AppError {
    /// A CLI flag that requires a value was provided without one.
    MissingValue(&'static str),
    /// A CLI value could not be parsed or failed validation.
    InvalidValue(&'static str),
    /// Usage text returned for unknown flags.
    Usage(String),
    /// JSON writer initialization failed.
    JsonWriter(io::Error),
    /// Explicit exit condition surfaced as an error instead of terminating.
    Exit(String),
}

#[cfg(feature = "demo-app")]
impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::MissingValue(flag) => write!(f, "{flag} requires value"),
            AppError::InvalidValue(detail) => write!(f, "Error during converting {detail}"),
            AppError::Usage(msg) | AppError::Exit(msg) => f.write_str(msg),
            AppError::JsonWriter(err) => err.fmt(f),
        }
    }
}

#[cfg(feature = "demo-app")]
impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::JsonWriter(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(feature = "demo-app")]
impl From<io::Error> for AppError {
    fn from(value: io::Error) -> Self {
        AppError::JsonWriter(value)
    }
}

/// Errors that can occur while running the sender/receiver loops.
#[derive(Debug)]
pub enum RunnerError {
    /// UDP socket operation failed.
    Socket(UdpSocketError),
    /// Packet buffer/view operation failed.
    Packet(PacketError),
    /// Sender startup timed out while waiting for the first trigger packet.
    StartupTriggerTimeout {
        /// Timeout budget used for the wait, in microseconds.
        waited_us: u32,
    },
    /// Sender aborted after consecutive timeouts, matching the reference behavior.
    ConsecutiveTimeouts,
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunnerError::Socket(err) => err.fmt(f),
            RunnerError::Packet(err) => err.fmt(f),
            RunnerError::StartupTriggerTimeout { waited_us } => {
                write!(
                    f,
                    "timed out after {waited_us} us while waiting for the startup trigger packet"
                )
            }
            RunnerError::ConsecutiveTimeouts => {
                f.write_str("stop prague sender due to consecutive timeout")
            }
        }
    }
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RunnerError::Socket(err) => Some(err),
            RunnerError::Packet(err) => Some(err),
            RunnerError::StartupTriggerTimeout { .. } => None,
            RunnerError::ConsecutiveTimeouts => None,
        }
    }
}

impl From<UdpSocketError> for RunnerError {
    fn from(value: UdpSocketError) -> Self {
        RunnerError::Socket(value)
    }
}

impl From<PacketError> for RunnerError {
    fn from(value: PacketError) -> Self {
        RunnerError::Packet(value)
    }
}

/// Errors that can occur while using the higher-level session wrapper APIs.
#[cfg(feature = "session")]
#[derive(Debug)]
pub enum SessionError {
    /// UDP socket operation failed.
    Socket(UdpSocketError),
    /// Packet view construction or parsing failed.
    Packet(PacketError),
    /// The app payload did not fit in the current Prague packet budget.
    PayloadTooLarge {
        /// Bytes requested by the caller.
        payload_len: usize,
        /// Maximum bytes that fit in the current datagram after Prague headers.
        max_payload_len: usize,
    },
    /// A video frame was queued while another one was still pending.
    FrameAlreadyQueued,
    /// The next video frame slot has not opened yet.
    FrameNotReady {
        /// Remaining delay until the next frame slot, in microseconds.
        next_frame_in_us: time_tp,
    },
    /// A blocking helper timed out while waiting for feedback needed to make progress.
    FeedbackTimeout {
        /// Timeout budget used for the wait, in microseconds.
        waited_us: time_tp,
        /// Current in-flight packet count when the timeout fired.
        inflight_packets: count_tp,
    },
    /// The sender is pacing-limited or window-limited and cannot send yet.
    WouldBlock {
        /// Remaining delay until the next pacing slot, in microseconds.
        next_send_in_us: time_tp,
        /// Current in-flight packet count.
        inflight_packets: count_tp,
        /// Current packet window limit.
        packet_window: count_tp,
    },
    /// A packet of an unexpected type was received for the current session role.
    UnexpectedPacketType(u8),
    /// A packet passed structural parsing but was invalid for the higher-level wrapper.
    InvalidPacket(&'static str),
}

#[cfg(feature = "session")]
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::Socket(err) => err.fmt(f),
            SessionError::Packet(err) => err.fmt(f),
            SessionError::PayloadTooLarge {
                payload_len,
                max_payload_len,
            } => write!(
                f,
                "payload too large: requested {payload_len} bytes but only {max_payload_len} bytes fit"
            ),
            SessionError::FrameAlreadyQueued => {
                f.write_str("a video frame is already queued")
            }
            SessionError::FrameNotReady { next_frame_in_us } => {
                write!(f, "next video frame slot opens in {next_frame_in_us} us")
            }
            SessionError::FeedbackTimeout {
                waited_us,
                inflight_packets,
            } => write!(
                f,
                "timed out after {waited_us} us while waiting for feedback (inflight {inflight_packets})"
            ),
            SessionError::WouldBlock {
                next_send_in_us,
                inflight_packets,
                packet_window,
            } => write!(
                f,
                "session would block (next send in {next_send_in_us} us, inflight {inflight_packets}/{packet_window})"
            ),
            SessionError::UnexpectedPacketType(ty) => {
                write!(f, "unexpected packet type 0x{ty:02x}")
            }
            SessionError::InvalidPacket(msg) => f.write_str(msg),
        }
    }
}

#[cfg(feature = "session")]
impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SessionError::Socket(err) => Some(err),
            SessionError::Packet(err) => Some(err),
            SessionError::PayloadTooLarge { .. }
            | SessionError::FrameAlreadyQueued
            | SessionError::FrameNotReady { .. }
            | SessionError::FeedbackTimeout { .. }
            | SessionError::WouldBlock { .. }
            | SessionError::UnexpectedPacketType(_)
            | SessionError::InvalidPacket(_) => None,
        }
    }
}

#[cfg(feature = "session")]
impl From<UdpSocketError> for SessionError {
    fn from(value: UdpSocketError) -> Self {
        SessionError::Socket(value)
    }
}

#[cfg(feature = "session")]
impl From<PacketError> for SessionError {
    fn from(value: PacketError) -> Self {
        SessionError::Packet(value)
    }
}
