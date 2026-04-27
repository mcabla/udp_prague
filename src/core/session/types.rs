use crate::congestion::{
    count_tp, ecn_tp, fps_tp, rate_tp, size_tp, time_tp, PragueRateAdvice, PragueVideoRateAdvice,
    PRAGUE_MAXRATE,
};
use crate::core::runtime::{FRAME_DURATION, FRAME_PER_SECOND};
use crate::core::SessionError;

const DEFAULT_MAX_PENDING_SEGMENTS: usize = 128;
const DEFAULT_MAX_PENDING_FRAMES: usize = 128;
const DEFAULT_MAX_PENDING_SEGMENT_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_PENDING_FRAME_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_MAX_PENDING_SEGMENT_AGE_US: time_tp = 10_000_000;
const DEFAULT_MAX_PENDING_FRAME_AGE_US: time_tp = 10_000_000;

/// Bounds for incomplete receiver-side reassembly state.
///
/// The segmented-bulk and video receiver wrappers keep incomplete logical
/// payloads across receive timeouts. These limits bound how many incomplete
/// items may be retained before the oldest incomplete item is evicted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueReceiverReassemblyLimits {
    /// Maximum number of incomplete segmented bulk payloads retained.
    pub max_pending_segments: usize,
    /// Maximum number of incomplete video frames retained.
    pub max_pending_frames: usize,
    /// Best-effort cap on retained segmented bulk chunk bytes across incomplete payloads.
    pub max_pending_segment_bytes: usize,
    /// Best-effort cap on retained RT frame fragment bytes across incomplete frames.
    pub max_pending_frame_bytes: usize,
    /// Maximum age of an incomplete segmented bulk payload before it is evicted.
    pub max_pending_segment_age_us: time_tp,
    /// Maximum age of an incomplete RT frame before it is evicted.
    pub max_pending_frame_age_us: time_tp,
}

impl PragueReceiverReassemblyLimits {
    pub(super) fn validate(self) -> Result<Self, SessionError> {
        if self.max_pending_segments == 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_segments must be greater than zero",
            ));
        }
        if self.max_pending_frames == 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_frames must be greater than zero",
            ));
        }
        if self.max_pending_segment_bytes == 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_segment_bytes must be greater than zero",
            ));
        }
        if self.max_pending_frame_bytes == 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_frame_bytes must be greater than zero",
            ));
        }
        if self.max_pending_segment_age_us <= 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_segment_age_us must be greater than zero",
            ));
        }
        if self.max_pending_frame_age_us <= 0 {
            return Err(SessionError::InvalidPacket(
                "max_pending_frame_age_us must be greater than zero",
            ));
        }
        Ok(self)
    }
}

impl Default for PragueReceiverReassemblyLimits {
    fn default() -> Self {
        Self {
            max_pending_segments: DEFAULT_MAX_PENDING_SEGMENTS,
            max_pending_frames: DEFAULT_MAX_PENDING_FRAMES,
            max_pending_segment_bytes: DEFAULT_MAX_PENDING_SEGMENT_BYTES,
            max_pending_frame_bytes: DEFAULT_MAX_PENDING_FRAME_BYTES,
            max_pending_segment_age_us: DEFAULT_MAX_PENDING_SEGMENT_AGE_US,
            max_pending_frame_age_us: DEFAULT_MAX_PENDING_FRAME_AGE_US,
        }
    }
}

/// Configuration for the higher-level session wrappers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSessionConfig {
    /// Maximum Prague packet size in bytes.
    pub max_packet_size: size_tp,
    /// Maximum allowed pacing rate in bytes per second.
    pub max_rate: rate_tp,
}

impl Default for PragueSessionConfig {
    fn default() -> Self {
        Self {
            max_packet_size: crate::congestion::PRAGUE_INITMTU,
            max_rate: PRAGUE_MAXRATE,
        }
    }
}

/// Configuration for the video-oriented sender session wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueVideoSessionConfig {
    /// Maximum Prague packet size in bytes.
    pub max_packet_size: size_tp,
    /// Maximum allowed pacing rate in bytes per second.
    pub max_rate: rate_tp,
    /// Target video frame rate.
    pub fps: fps_tp,
    /// Per-frame send budget in microseconds.
    pub frame_budget_us: time_tp,
}

impl Default for PragueVideoSessionConfig {
    fn default() -> Self {
        Self {
            max_packet_size: crate::congestion::PRAGUE_INITMTU,
            max_rate: PRAGUE_MAXRATE,
            fps: FRAME_PER_SECOND,
            frame_budget_us: FRAME_DURATION as time_tp,
        }
    }
}

/// Result of sending one application datagram with Prague metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSendReport {
    /// Prague sequence number assigned to the datagram.
    pub sequence_number: count_tp,
    /// Total datagram size sent on the wire.
    pub total_bytes: size_tp,
    /// Application bytes carried after the Prague header.
    pub app_data_len: usize,
    /// Current pacing and congestion advice at send time.
    pub advice: PragueRateAdvice,
}

/// Summary of a blocking large-payload bulk transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueBulkTransferReport {
    /// Number of Prague bulk packets sent for this payload.
    pub packets_sent: u32,
    /// Total application bytes consumed from the caller payload.
    pub app_bytes_sent: size_tp,
    /// Total bytes sent on the wire across all packets.
    pub bytes_sent_on_wire: size_tp,
    /// Sequence number of the final Prague packet sent for this payload.
    pub last_sequence_number: Option<count_tp>,
    /// Number of ACK packets processed while completing the transfer.
    pub feedback_packets_processed: u32,
    /// Remaining in-flight Prague packets when the helper returned.
    pub inflight_packets: count_tp,
    /// Fresh pacing and congestion guidance after the transfer completed.
    pub advice: PragueRateAdvice,
}

/// Summary of a high-level segmented bulk transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueSegmentSendReport {
    /// Logical application content tag carried with every chunk.
    pub content_tag: u16,
    /// Logical segment identifier assigned by the sender wrapper.
    pub segment_id: u32,
    /// Number of Prague bulk packets emitted for this segment.
    pub packets_sent: u32,
    /// Total application payload bytes carried by the logical segment.
    pub segment_size_bytes: size_tp,
    /// Total bytes sent on the wire across all Prague packets.
    pub bytes_sent_on_wire: size_tp,
    /// Sequence number of the last Prague packet emitted for this segment.
    pub last_sequence_number: Option<count_tp>,
    /// Number of ACK packets processed while completing the transfer.
    pub feedback_packets_processed: u32,
    /// Fresh pacing and congestion guidance after the transfer completed.
    pub advice: PragueRateAdvice,
}

/// One fully reassembled logical payload received through segmented bulk mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragueReceivedSegment {
    /// Application-defined content tag carried by the sender.
    pub content_tag: u16,
    /// Sender-assigned logical segment identifier.
    pub segment_id: u32,
    /// Reassembled logical payload.
    pub payload: Vec<u8>,
}

/// One fully reassembled RT/video frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragueReceivedVideoFrame {
    /// Sender-assigned Prague frame number.
    pub frame_number: count_tp,
    /// Total reassembled frame size in bytes.
    pub frame_size_bytes: size_tp,
    /// Fully reassembled encoded frame payload.
    pub payload: Vec<u8>,
}

/// Result of queueing a video frame for paced transmission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueQueuedVideoFrame {
    /// Assigned Prague frame number.
    pub frame_number: count_tp,
    /// Actual application frame size in bytes.
    pub actual_frame_size_bytes: size_tp,
    /// Current Prague target frame size in bytes.
    pub target_frame_size_bytes: size_tp,
    /// Whether the queued frame exceeds the current Prague target.
    pub over_target: bool,
    /// Current Prague video advice for this frame slot.
    pub advice: PragueVideoRateAdvice,
}

/// Result of sending one paced batch of frame fragments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueVideoSendReport {
    /// Prague frame number being sent.
    pub frame_number: count_tp,
    /// Number of fragments sent in this batch.
    pub fragments_sent: u32,
    /// Total bytes sent on the wire in this batch.
    pub bytes_sent_on_wire: size_tp,
    /// Total application payload bytes sent in this batch.
    pub app_bytes_sent: size_tp,
    /// Remaining application payload bytes for the current frame.
    pub remaining_app_bytes: size_tp,
    /// Sequence number of the last fragment sent in this batch.
    pub last_sequence_number: count_tp,
    /// Whether the frame finished transmitting in this batch.
    pub frame_complete: bool,
    /// Fresh Prague video advice after sending.
    pub advice: PragueVideoRateAdvice,
}

/// Result of processing one ACK at the sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueAckFeedback {
    /// Sequence number acknowledged by the peer.
    pub acked_sequence_number: count_tp,
    /// Bytes received for the ACK packet.
    pub bytes_received: size_tp,
    /// Total packets received reported by the peer.
    pub packets_received: count_tp,
    /// Total CE marks reported by the peer.
    pub packets_ce: count_tp,
    /// Total lost packets reported by the peer.
    pub packets_lost: count_tp,
    /// Whether the peer requested fallback from L4S marking.
    pub error_l4s: bool,
    /// Current sender-side in-flight packet count after processing the ACK.
    pub inflight_packets: count_tp,
    /// Fresh pacing and congestion advice after processing the ACK.
    pub advice: PragueRateAdvice,
}

/// Result of processing one ACK for video/frame traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueVideoAckFeedback {
    /// Sequence number acknowledged by the peer.
    pub acked_sequence_number: count_tp,
    /// Bytes received for the ACK packet.
    pub bytes_received: size_tp,
    /// Total packets received reported by the peer.
    pub packets_received: count_tp,
    /// Total CE marks reported by the peer.
    pub packets_ce: count_tp,
    /// Total lost packets reported by the peer.
    pub packets_lost: count_tp,
    /// Whether the peer requested fallback from L4S marking.
    pub error_l4s: bool,
    /// Current sender-side in-flight packet count after processing the ACK.
    pub inflight_packets: count_tp,
    /// Current sender-side in-flight frame count after processing the ACK.
    pub inflight_frames: count_tp,
    /// Total frames fully sent so far.
    pub sent_frames: count_tp,
    /// Total frames fully received so far.
    pub received_frames: count_tp,
    /// Total frames considered lost so far.
    pub lost_frames: count_tp,
    /// Fresh Prague video advice after processing the ACK.
    pub advice: PragueVideoRateAdvice,
}

/// Parsed bulk packet delivered to the receiver application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragueReceivedBulkPacket {
    /// Prague sequence number.
    pub sequence_number: count_tp,
    /// Prague timestamp.
    pub timestamp: time_tp,
    /// Echoed timestamp used for RTT estimation.
    pub echoed_timestamp: time_tp,
    /// ECN value observed on the incoming packet.
    pub ecn: ecn_tp,
    /// Application bytes after the Prague data header.
    pub app_data: Vec<u8>,
}

/// Parsed frame packet delivered to the receiver application.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragueReceivedFramePacket {
    /// Prague sequence number.
    pub sequence_number: count_tp,
    /// Prague timestamp.
    pub timestamp: time_tp,
    /// Echoed timestamp used for RTT estimation.
    pub echoed_timestamp: time_tp,
    /// Frame number carried in the Prague frame header.
    pub frame_number: count_tp,
    /// Byte offset of this fragment inside the frame.
    pub frame_offset_bytes: count_tp,
    /// Total frame size in bytes.
    pub frame_size_bytes: count_tp,
    /// ECN value observed on the incoming packet.
    pub ecn: ecn_tp,
    /// Application bytes after the Prague frame header.
    pub app_data: Vec<u8>,
}

/// Application-facing parsed inbound Prague packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PragueReceivedPacket {
    Bulk(PragueReceivedBulkPacket),
    Frame(PragueReceivedFramePacket),
}

impl PragueReceivedPacket {
    /// Sequence number of the parsed packet.
    pub fn sequence_number(&self) -> count_tp {
        match self {
            PragueReceivedPacket::Bulk(packet) => packet.sequence_number,
            PragueReceivedPacket::Frame(packet) => packet.sequence_number,
        }
    }
}

/// Borrowed bulk packet view delivered directly from the receiver buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueReceivedBulkPacketView<'a> {
    /// Prague sequence number.
    pub sequence_number: count_tp,
    /// Prague timestamp.
    pub timestamp: time_tp,
    /// Echoed timestamp used for RTT estimation.
    pub echoed_timestamp: time_tp,
    /// ECN value observed on the incoming packet.
    pub ecn: ecn_tp,
    /// Application bytes after the Prague data header.
    pub app_data: &'a [u8],
}

/// Borrowed frame packet view delivered directly from the receiver buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueReceivedFramePacketView<'a> {
    /// Prague sequence number.
    pub sequence_number: count_tp,
    /// Prague timestamp.
    pub timestamp: time_tp,
    /// Echoed timestamp used for RTT estimation.
    pub echoed_timestamp: time_tp,
    /// Frame number carried in the Prague frame header.
    pub frame_number: count_tp,
    /// Byte offset of this fragment inside the frame.
    pub frame_offset_bytes: count_tp,
    /// Total frame size in bytes.
    pub frame_size_bytes: count_tp,
    /// ECN value observed on the incoming packet.
    pub ecn: ecn_tp,
    /// Application bytes after the Prague frame header.
    pub app_data: &'a [u8],
}

/// Borrowed application-facing parsed inbound Prague packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PragueReceivedPacketView<'a> {
    Bulk(PragueReceivedBulkPacketView<'a>),
    Frame(PragueReceivedFramePacketView<'a>),
}

impl PragueReceivedPacketView<'_> {
    /// Sequence number of the parsed packet.
    pub fn sequence_number(&self) -> count_tp {
        match self {
            PragueReceivedPacketView::Bulk(packet) => packet.sequence_number,
            PragueReceivedPacketView::Frame(packet) => packet.sequence_number,
        }
    }

    /// Clone the borrowed packet data into the existing owned packet shape.
    pub fn to_owned(&self) -> PragueReceivedPacket {
        match self {
            PragueReceivedPacketView::Bulk(packet) => {
                PragueReceivedPacket::Bulk(PragueReceivedBulkPacket {
                    sequence_number: packet.sequence_number,
                    timestamp: packet.timestamp,
                    echoed_timestamp: packet.echoed_timestamp,
                    ecn: packet.ecn,
                    app_data: packet.app_data.to_vec(),
                })
            }
            PragueReceivedPacketView::Frame(packet) => {
                PragueReceivedPacket::Frame(PragueReceivedFramePacket {
                    sequence_number: packet.sequence_number,
                    timestamp: packet.timestamp,
                    echoed_timestamp: packet.echoed_timestamp,
                    frame_number: packet.frame_number,
                    frame_offset_bytes: packet.frame_offset_bytes,
                    frame_size_bytes: packet.frame_size_bytes,
                    ecn: packet.ecn,
                    app_data: packet.app_data.to_vec(),
                })
            }
        }
    }
}

/// Result of sending one ACK from the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueAckReport {
    /// Prague sequence number being acknowledged.
    pub acked_sequence_number: count_tp,
    /// Total ACK packet size sent.
    pub bytes_sent: size_tp,
    /// Total packets received reported to the sender.
    pub packets_received: count_tp,
    /// Total CE marks reported to the sender.
    pub packets_ce: count_tp,
    /// Total losses reported to the sender.
    pub packets_lost: count_tp,
    /// Whether the receiver requests L4S fallback.
    pub error_l4s: bool,
    /// ECN value used on the ACK packet.
    pub next_send_ecn: ecn_tp,
}

/// Convenience result combining an inbound packet with the ACK that was sent for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PragueReceivedPacketAndAck {
    pub packet: PragueReceivedPacket,
    pub ack: PragueAckReport,
}

/// Borrowed convenience result combining an inbound packet view with the ACK sent for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PragueReceivedPacketAndAckView<'a> {
    pub packet: PragueReceivedPacketView<'a>,
    pub ack: PragueAckReport,
}

impl PragueReceivedPacketAndAckView<'_> {
    /// Clone the borrowed packet view into the existing owned packet-and-ack shape.
    pub fn to_owned(&self) -> PragueReceivedPacketAndAck {
        PragueReceivedPacketAndAck {
            packet: self.packet.to_owned(),
            ack: self.ack,
        }
    }
}
