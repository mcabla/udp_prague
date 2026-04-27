use std::collections::BTreeMap;

use crate::congestion::{count_tp, size_tp, time_tp, PragueRateAdvice};
use crate::core::SessionError;

use super::receiver::PragueReceiverSession;
use super::sender::PragueSenderSession;
use super::sleep_delay_us;
use super::types::{
    PragueReceivedPacketView, PragueReceivedSegment, PragueReceiverReassemblyLimits,
    PragueSegmentSendReport, PragueSessionConfig,
};

const SEGMENT_BULK_MAGIC: [u8; 4] = *b"UPSG";
const SEGMENT_BULK_VERSION: u8 = 1;
const SEGMENT_BULK_HEADER_SIZE: usize = 28;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PragueSegmentBulkHeader {
    content_tag: u16,
    segment_id: u32,
    segment_offset_bytes: u64,
    segment_size_bytes: u64,
}

impl PragueSegmentBulkHeader {
    fn encode_into(self, buffer: &mut [u8]) {
        buffer[..4].copy_from_slice(&SEGMENT_BULK_MAGIC);
        buffer[4] = SEGMENT_BULK_VERSION;
        buffer[5] = 0;
        buffer[6..8].copy_from_slice(&self.content_tag.to_be_bytes());
        buffer[8..12].copy_from_slice(&self.segment_id.to_be_bytes());
        buffer[12..20].copy_from_slice(&self.segment_offset_bytes.to_be_bytes());
        buffer[20..28].copy_from_slice(&self.segment_size_bytes.to_be_bytes());
    }

    fn decode(buffer: &[u8]) -> Result<(Self, &[u8]), SessionError> {
        if buffer.len() < SEGMENT_BULK_HEADER_SIZE {
            return Err(SessionError::InvalidPacket(
                "segmented bulk payload too small for header",
            ));
        }
        if buffer[..4] != SEGMENT_BULK_MAGIC {
            return Err(SessionError::InvalidPacket(
                "segmented bulk payload missing magic",
            ));
        }
        if buffer[4] != SEGMENT_BULK_VERSION {
            return Err(SessionError::InvalidPacket(
                "unsupported segmented bulk payload version",
            ));
        }

        let header = Self {
            content_tag: u16::from_be_bytes([buffer[6], buffer[7]]),
            segment_id: u32::from_be_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]),
            segment_offset_bytes: u64::from_be_bytes([
                buffer[12], buffer[13], buffer[14], buffer[15], buffer[16], buffer[17], buffer[18],
                buffer[19],
            ]),
            segment_size_bytes: u64::from_be_bytes([
                buffer[20], buffer[21], buffer[22], buffer[23], buffer[24], buffer[25], buffer[26],
                buffer[27],
            ]),
        };
        let chunk = &buffer[SEGMENT_BULK_HEADER_SIZE..];
        if header.segment_offset_bytes > header.segment_size_bytes {
            return Err(SessionError::InvalidPacket(
                "segment chunk starts beyond declared segment size",
            ));
        }
        if chunk.len() as u64 > header.segment_size_bytes - header.segment_offset_bytes {
            return Err(SessionError::InvalidPacket(
                "segment chunk exceeds declared segment size",
            ));
        }

        Ok((header, chunk))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingBulkSegment {
    content_tag: u16,
    total_len: usize,
    received_len: usize,
    chunks: BTreeMap<usize, Vec<u8>>,
    last_update_order: u64,
    last_update_time: time_tp,
}

impl PendingBulkSegment {
    fn new(
        content_tag: u16,
        total_len: usize,
        last_update_order: u64,
        last_update_time: time_tp,
    ) -> Self {
        Self {
            content_tag,
            total_len,
            received_len: 0,
            chunks: BTreeMap::new(),
            last_update_order,
            last_update_time,
        }
    }

    fn retained_bytes(&self) -> usize {
        self.received_len
    }

    fn validate_metadata(&self, content_tag: u16, total_len: usize) -> Result<(), SessionError> {
        if self.content_tag != content_tag || self.total_len != total_len {
            return Err(SessionError::InvalidPacket(
                "conflicting segmented bulk metadata for existing segment id",
            ));
        }
        Ok(())
    }

    fn additional_bytes_for_chunk(
        &self,
        offset: usize,
        chunk: &[u8],
    ) -> Result<usize, SessionError> {
        if offset > self.total_len || chunk.len() > self.total_len.saturating_sub(offset) {
            return Err(SessionError::InvalidPacket(
                "segmented bulk chunk exceeds declared bounds",
            ));
        }

        if let Some(existing) = self.chunks.get(&offset) {
            if existing.as_slice() == chunk {
                return Ok(0);
            }
            return Err(SessionError::InvalidPacket(
                "conflicting duplicate segmented bulk chunk",
            ));
        }

        if let Some((previous_offset, previous_chunk)) = self.chunks.range(..offset).next_back() {
            if previous_offset + previous_chunk.len() > offset {
                return Err(SessionError::InvalidPacket(
                    "overlapping segmented bulk chunk",
                ));
            }
        }
        if let Some((next_offset, _)) = self.chunks.range(offset..).next() {
            if offset + chunk.len() > *next_offset {
                return Err(SessionError::InvalidPacket(
                    "overlapping segmented bulk chunk",
                ));
            }
        }

        Ok(chunk.len())
    }

    #[cfg(test)]
    fn insert_chunk(&mut self, offset: usize, chunk: &[u8]) -> Result<(), SessionError> {
        self.insert_chunk_owned(offset, chunk.to_vec())
    }

    fn insert_chunk_owned(&mut self, offset: usize, chunk: Vec<u8>) -> Result<(), SessionError> {
        let additional_bytes = self.additional_bytes_for_chunk(offset, &chunk)?;
        if additional_bytes == 0 {
            return Ok(());
        }

        self.received_len = self.received_len.saturating_add(additional_bytes);
        self.chunks.insert(offset, chunk);
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.received_len == self.total_len && (self.total_len == 0 || !self.chunks.is_empty())
    }

    fn into_payload(self) -> Result<Vec<u8>, SessionError> {
        if self.total_len == 0 {
            return Ok(Vec::new());
        }

        let mut payload = Vec::with_capacity(self.total_len);
        let mut next_offset = 0usize;
        for (offset, chunk) in self.chunks {
            if offset != next_offset {
                return Err(SessionError::InvalidPacket(
                    "segmented bulk payload has a gap during reassembly",
                ));
            }
            payload.extend_from_slice(&chunk);
            next_offset = next_offset.saturating_add(chunk.len());
        }

        if next_offset != self.total_len {
            return Err(SessionError::InvalidPacket(
                "segmented bulk payload ended before declared size",
            ));
        }

        Ok(payload)
    }
}

/// Higher-level sender wrapper for Rust-only segmented bulk payloads.
pub struct PragueSegmentSenderSession {
    inner: PragueSenderSession,
    next_segment_id: u32,
}

/// Higher-level receiver wrapper that reassembles segmented bulk payloads.
pub struct PragueSegmentReceiverSession {
    inner: PragueReceiverSession,
    pending_segments: BTreeMap<u32, PendingBulkSegment>,
    reassembly_limits: PragueReceiverReassemblyLimits,
    next_pending_order: u64,
}

impl PragueSegmentSenderSession {
    /// Open a connected segmented-bulk sender session to a peer.
    pub fn connect(
        addr: &str,
        port: u16,
        config: PragueSessionConfig,
    ) -> Result<Self, SessionError> {
        Ok(Self {
            inner: PragueSenderSession::connect(addr, port, config)?,
            next_segment_id: 0,
        })
    }

    /// Current application-facing pacing and congestion guidance.
    pub fn advice(&mut self) -> PragueRateAdvice {
        self.inner.advice()
    }

    /// Current recommended sender bitrate in bits per second.
    pub fn recommended_bitrate_bits_per_sec(&mut self) -> u64 {
        self.inner.recommended_bitrate_bits_per_sec()
    }

    /// Configured sender bitrate cap in bits per second.
    pub fn max_configured_bitrate_bits_per_sec(&self) -> u64 {
        self.inner.max_configured_bitrate_bits_per_sec()
    }

    /// Current in-flight Prague packet count.
    pub fn inflight_packets(&self) -> count_tp {
        self.inner.inflight_packets()
    }

    /// Send one logical payload by chunking it across ordinary Prague bulk packets.
    pub fn send_segment_blocking(
        &mut self,
        content_tag: u16,
        payload: &[u8],
        feedback_timeout_us: time_tp,
    ) -> Result<PragueSegmentSendReport, SessionError> {
        if feedback_timeout_us <= 0 {
            return Err(SessionError::InvalidPacket(
                "feedback timeout must be > 0 for segmented bulk transfer",
            ));
        }

        let segment_size_bytes = payload.len() as size_tp;
        if segment_size_bytes as usize != payload.len() {
            return Err(SessionError::InvalidPacket(
                "segmented bulk payload too large for this platform",
            ));
        }

        let segment_id = self.next_segment_id.wrapping_add(1);
        self.next_segment_id = segment_id;

        let mut report = PragueSegmentSendReport {
            content_tag,
            segment_id,
            packets_sent: 0,
            segment_size_bytes,
            bytes_sent_on_wire: 0,
            last_sequence_number: None,
            feedback_packets_processed: 0,
            advice: self.inner.advice(),
        };
        let mut offset = 0usize;
        let mut emitted_empty_chunk = false;

        while offset < payload.len()
            || (!emitted_empty_chunk && payload.is_empty())
            || self.inner.inflight_packets() > 0
        {
            while (offset < payload.len() || (!emitted_empty_chunk && payload.is_empty()))
                && self.inner.can_send_now()
            {
                let packet_budget = self.inner.max_app_data_len();
                if packet_budget <= SEGMENT_BULK_HEADER_SIZE {
                    return Err(SessionError::InvalidPacket(
                        "bulk packet budget too small for segmented bulk header",
                    ));
                }

                let chunk_payload_budget = packet_budget - SEGMENT_BULK_HEADER_SIZE;
                let chunk_len = if payload.is_empty() && !emitted_empty_chunk {
                    0
                } else {
                    chunk_payload_budget.min(payload.len() - offset)
                };

                let mut header = [0u8; SEGMENT_BULK_HEADER_SIZE];
                PragueSegmentBulkHeader {
                    content_tag,
                    segment_id,
                    segment_offset_bytes: offset as u64,
                    segment_size_bytes,
                }
                .encode_into(&mut header);

                let sent = self
                    .inner
                    .send_bulk_parts(&header, &payload[offset..offset + chunk_len])?;
                offset = offset.saturating_add(chunk_len);
                emitted_empty_chunk |= payload.is_empty();

                report.packets_sent = report.packets_sent.wrapping_add(1);
                report.bytes_sent_on_wire =
                    report.bytes_sent_on_wire.wrapping_add(sent.total_bytes);
                report.last_sequence_number = Some(sent.sequence_number);
                report.advice = sent.advice;
            }

            if offset >= payload.len()
                && (emitted_empty_chunk || !payload.is_empty())
                && self.inner.inflight_packets() == 0
            {
                break;
            }

            if (offset < payload.len() || (!emitted_empty_chunk && payload.is_empty()))
                && self.inner.inflight_packets() == 0
            {
                sleep_delay_us(self.inner.next_send_delay_us());
                continue;
            }

            match self.inner.receive_feedback(feedback_timeout_us)? {
                Some(feedback) => {
                    report.feedback_packets_processed =
                        report.feedback_packets_processed.wrapping_add(1);
                    report.advice = feedback.advice;
                }
                None => {
                    if offset >= payload.len()
                        && (emitted_empty_chunk || !payload.is_empty())
                        && self.inner.inflight_packets() == 0
                    {
                        break;
                    }
                    return Err(SessionError::FeedbackTimeout {
                        waited_us: feedback_timeout_us,
                        inflight_packets: self.inner.inflight_packets(),
                    });
                }
            }
        }

        report.advice = self.inner.advice();
        Ok(report)
    }
}

impl PragueSegmentReceiverSession {
    /// Bind a segmented-bulk receiver session to a local address.
    pub fn bind(addr: &str, port: u16) -> Result<Self, SessionError> {
        Self::bind_with_limits(addr, port, PragueReceiverReassemblyLimits::default())
    }

    /// Bind a segmented-bulk receiver session with explicit reassembly limits.
    pub fn bind_with_limits(
        addr: &str,
        port: u16,
        limits: PragueReceiverReassemblyLimits,
    ) -> Result<Self, SessionError> {
        let limits = limits.validate()?;
        Ok(Self {
            inner: PragueReceiverSession::bind(addr, port)?,
            pending_segments: BTreeMap::new(),
            reassembly_limits: limits,
            next_pending_order: 0,
        })
    }

    /// Current receiver-side congestion view.
    pub fn advice(&mut self) -> PragueRateAdvice {
        self.inner.advice()
    }

    fn pending_segment_bytes(&self) -> usize {
        self.pending_segments
            .values()
            .map(PendingBulkSegment::retained_bytes)
            .sum()
    }

    fn evict_oldest_pending_segment_except(&mut self, keep_segment_id: Option<u32>) -> bool {
        let Some(segment_id) = self
            .pending_segments
            .iter()
            .filter(|(segment_id, _)| keep_segment_id != Some(**segment_id))
            .min_by_key(|(_, pending)| pending.last_update_order)
            .map(|(segment_id, _)| *segment_id)
        else {
            return false;
        };
        self.pending_segments.remove(&segment_id);
        true
    }

    fn evict_stale_pending_segments(&mut self, now: time_tp, keep_segment_id: Option<u32>) {
        let max_age_us = self.reassembly_limits.max_pending_segment_age_us;
        let stale_segment_ids: Vec<u32> = self
            .pending_segments
            .iter()
            .filter(|(segment_id, pending)| {
                keep_segment_id != Some(**segment_id)
                    && now.wrapping_sub(pending.last_update_time) > max_age_us
            })
            .map(|(segment_id, _)| *segment_id)
            .collect();

        for segment_id in stale_segment_ids {
            self.pending_segments.remove(&segment_id);
        }
    }

    fn prune_pending_segments(
        &mut self,
        now: time_tp,
        keep_segment_id: Option<u32>,
        additional_bytes: usize,
    ) {
        self.evict_stale_pending_segments(now, keep_segment_id);

        if !keep_segment_id
            .is_some_and(|segment_id| self.pending_segments.contains_key(&segment_id))
        {
            while self.pending_segments.len() >= self.reassembly_limits.max_pending_segments {
                if !self.evict_oldest_pending_segment_except(keep_segment_id) {
                    break;
                }
            }
        }

        while additional_bytes > 0
            && self
                .pending_segment_bytes()
                .saturating_add(additional_bytes)
                > self.reassembly_limits.max_pending_segment_bytes
        {
            if !self.evict_oldest_pending_segment_except(keep_segment_id) {
                break;
            }
        }
    }

    /// Receive Prague bulk packets, ACK each one, and return a logical segment when complete.
    ///
    /// If the timeout expires before the next packet arrives, returns `Ok(None)` and keeps any
    /// partially reassembled segments for a future call.
    pub fn receive_segment_and_ack(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedSegment>, SessionError> {
        loop {
            let received = match self.inner.receive_and_ack_borrowed(timeout)? {
                Some(received) => received,
                None => {
                    let now = self.inner.now();
                    self.prune_pending_segments(now, None, 0);
                    return Ok(None);
                }
            };

            let (header, total_len, offset, chunk) = {
                let packet = match received.packet {
                    PragueReceivedPacketView::Bulk(packet) => packet,
                    PragueReceivedPacketView::Frame(_) => {
                        return Err(SessionError::InvalidPacket(
                            "segmented bulk receiver does not accept frame packets",
                        ))
                    }
                };

                let (header, chunk) = PragueSegmentBulkHeader::decode(packet.app_data)?;
                let total_len = header.segment_size_bytes as usize;
                let offset = header.segment_offset_bytes as usize;
                if total_len as u64 != header.segment_size_bytes
                    || offset as u64 != header.segment_offset_bytes
                {
                    return Err(SessionError::InvalidPacket(
                        "segmented bulk payload exceeds platform usize range",
                    ));
                }

                (header, total_len, offset, chunk.to_vec())
            };

            let order = self.next_pending_order;
            self.next_pending_order = self.next_pending_order.wrapping_add(1);
            let now = self.inner.now();
            let additional_bytes = match self.pending_segments.get(&header.segment_id) {
                Some(pending) => {
                    pending.validate_metadata(header.content_tag, total_len)?;
                    pending.additional_bytes_for_chunk(offset, &chunk)?
                }
                None => chunk.len(),
            };
            self.prune_pending_segments(now, Some(header.segment_id), additional_bytes);
            let mut completed_segment = None;
            {
                let pending = self
                    .pending_segments
                    .entry(header.segment_id)
                    .or_insert_with(|| {
                        PendingBulkSegment::new(header.content_tag, total_len, order, now)
                    });
                pending.validate_metadata(header.content_tag, total_len)?;
                pending.insert_chunk_owned(offset, chunk)?;
                pending.last_update_order = order;
                pending.last_update_time = now;
                if pending.is_complete() {
                    completed_segment = self.pending_segments.remove(&header.segment_id);
                }
            }

            if let Some(segment) = completed_segment {
                return Ok(Some(PragueReceivedSegment {
                    content_tag: segment.content_tag,
                    segment_id: header.segment_id,
                    payload: segment.into_payload()?,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmented_bulk_header_roundtrip_validates_bounds() {
        let header = PragueSegmentBulkHeader {
            content_tag: 7,
            segment_id: 42,
            segment_offset_bytes: 12,
            segment_size_bytes: 128,
        };
        let mut encoded = vec![0u8; SEGMENT_BULK_HEADER_SIZE + 8];
        header.encode_into(&mut encoded[..SEGMENT_BULK_HEADER_SIZE]);

        let (decoded, chunk) = PragueSegmentBulkHeader::decode(&encoded).expect("decode header");
        assert_eq!(decoded, header);
        assert_eq!(chunk.len(), 8);
    }

    #[test]
    fn receiver_reassembly_limits_reject_zero_caps() {
        let err = PragueSegmentReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_segments: 0,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .err()
        .expect("zero segment cap should be rejected");
        match err {
            SessionError::InvalidPacket(msg) => {
                assert_eq!(msg, "max_pending_segments must be greater than zero")
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn segmented_receiver_evicts_oldest_incomplete_segment_when_limit_is_hit() {
        let mut receiver = PragueSegmentReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_segments: 1,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("segment receiver");

        receiver
            .pending_segments
            .insert(1, PendingBulkSegment::new(7, 64, 0, 1));
        receiver.prune_pending_segments(1, Some(2), 1);

        assert!(!receiver.pending_segments.contains_key(&1));
        assert!(receiver.pending_segments.is_empty());
    }

    #[test]
    fn segmented_receiver_evicts_stale_incomplete_segment() {
        let mut receiver = PragueSegmentReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_segment_age_us: 10,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("segment receiver");

        receiver
            .pending_segments
            .insert(1, PendingBulkSegment::new(7, 64, 0, 5));
        receiver.prune_pending_segments(16, None, 0);

        assert!(receiver.pending_segments.is_empty());
    }

    #[test]
    fn segmented_receiver_evicts_oldest_segment_when_byte_budget_is_hit() {
        let mut receiver = PragueSegmentReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_segment_bytes: 7,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("segment receiver");

        let mut older = PendingBulkSegment::new(7, 16, 0, 1);
        older.insert_chunk(0, b"abcd").expect("older chunk");
        receiver.pending_segments.insert(1, older);

        let mut newer = PendingBulkSegment::new(7, 16, 1, 2);
        newer.insert_chunk(0, b"efg").expect("newer chunk");
        receiver.pending_segments.insert(2, newer);

        receiver.prune_pending_segments(2, Some(2), 1);

        assert!(!receiver.pending_segments.contains_key(&1));
        assert!(receiver.pending_segments.contains_key(&2));
        assert_eq!(receiver.pending_segment_bytes(), 3);
    }
}
