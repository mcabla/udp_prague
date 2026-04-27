use std::collections::BTreeMap;

use crate::congestion::{
    count_tp, ecn_tp, fps_tp, rate_tp, size_tp, time_tp, PragueCC, PragueRateAdvice,
    PragueVideoRateAdvice, PRAGUE_INITRATE, PRAGUE_INITWIN, PRAGUE_MINRATE,
};
use crate::core::SessionError;
use crate::net::UDPSocket;
use crate::protocol::pkt_format::{
    pktsend_tp, AckMessage, FrameMessage, BUFFER_SIZE, FRM_BUFFER_SIZE, PKT_ACK_TYPE,
    PKT_BUFFER_SIZE,
};

use super::receiver::PragueReceiverSession;
use super::types::{
    PragueQueuedVideoFrame, PragueReceivedPacketView, PragueReceivedVideoFrame,
    PragueReceiverReassemblyLimits, PragueVideoAckFeedback, PragueVideoSendReport,
    PragueVideoSessionConfig,
};
use super::{bool_as_count, boxed_array, idx_frm};

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingVideoFrame {
    frame_number: count_tp,
    frame_data: Vec<u8>,
    sent_app_bytes: usize,
    advice: PragueVideoRateAdvice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingVideoFramePayload {
    total_len: usize,
    received_len: usize,
    chunks: BTreeMap<usize, Vec<u8>>,
    last_update_order: u64,
    last_update_time: time_tp,
}

impl PendingVideoFramePayload {
    fn new(total_len: usize, last_update_order: u64, last_update_time: time_tp) -> Self {
        Self {
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

    fn validate_total_len(&self, total_len: usize) -> Result<(), SessionError> {
        if self.total_len != total_len {
            return Err(SessionError::InvalidPacket(
                "conflicting RT frame size for existing frame number",
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
                "RT frame fragment exceeds declared frame bounds",
            ));
        }

        if let Some(existing) = self.chunks.get(&offset) {
            if existing.as_slice() == chunk {
                return Ok(0);
            }
            return Err(SessionError::InvalidPacket(
                "conflicting duplicate RT frame fragment",
            ));
        }

        if let Some((previous_offset, previous_chunk)) = self.chunks.range(..offset).next_back() {
            if previous_offset + previous_chunk.len() > offset {
                return Err(SessionError::InvalidPacket(
                    "overlapping RT frame fragments",
                ));
            }
        }
        if let Some((next_offset, _)) = self.chunks.range(offset..).next() {
            if offset + chunk.len() > *next_offset {
                return Err(SessionError::InvalidPacket(
                    "overlapping RT frame fragments",
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
                    "RT frame payload has a gap during reassembly",
                ));
            }
            payload.extend_from_slice(&chunk);
            next_offset = next_offset.saturating_add(chunk.len());
        }

        if next_offset != self.total_len {
            return Err(SessionError::InvalidPacket(
                "RT frame payload ended before declared size",
            ));
        }

        Ok(payload)
    }
}

/// Sender-side wrapper for Prague RT/frame-oriented traffic.
pub struct PragueVideoSenderSession {
    socket: UDPSocket,
    cc: PragueCC,
    receive_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    sendtime: Box<[time_tp; PKT_BUFFER_SIZE]>,
    packet_state: Box<[pktsend_tp; PKT_BUFFER_SIZE]>,
    frame_idx: Box<[count_tp; PKT_BUFFER_SIZE]>,
    frame_pktlost: Box<[count_tp; FRM_BUFFER_SIZE]>,
    frame_pktsent: Box<[count_tp; FRM_BUFFER_SIZE]>,
    next_send: time_tp,
    comp_recv: time_tp,
    sequence_number: count_tp,
    inflight_packets: count_tp,
    lost_packets_state: count_tp,
    frame_number: count_tp,
    frame_inflight: count_tp,
    sent_frames: count_tp,
    received_frames: count_tp,
    lost_frames: count_tp,
    frame_timer: time_tp,
    fps: fps_tp,
    frame_interval_us: time_tp,
    pending_frame: Option<PendingVideoFrame>,
}

/// Higher-level receiver wrapper that reassembles full RT/video frames.
pub struct PragueVideoReceiverSession {
    inner: PragueReceiverSession,
    pending_frames: BTreeMap<count_tp, PendingVideoFramePayload>,
    reassembly_limits: PragueReceiverReassemblyLimits,
    next_pending_order: u64,
}

impl PragueVideoSenderSession {
    /// Open a connected RT/frame sender session to a peer.
    pub fn connect(
        addr: &str,
        port: u16,
        config: PragueVideoSessionConfig,
    ) -> Result<Self, SessionError> {
        if config.fps == 0 {
            return Err(SessionError::InvalidPacket("video fps must be > 0"));
        }
        if config.frame_budget_us <= 0 {
            return Err(SessionError::InvalidPacket(
                "video frame budget must be > 0",
            ));
        }

        let mut socket = UDPSocket::new();
        socket.Connect(addr, port)?;

        let mut cc = PragueCC::new(
            config.max_packet_size,
            config.fps,
            config.frame_budget_us,
            PRAGUE_INITRATE,
            PRAGUE_INITWIN,
            PRAGUE_MINRATE,
            config.max_rate,
        );
        let next_send = cc.Now();
        let frame_interval_us = (1_000_000u32 / u32::from(config.fps.max(1))) as time_tp;

        Ok(Self {
            socket,
            cc,
            receive_buffer: vec![0u8; BUFFER_SIZE.max(config.max_packet_size as usize)],
            send_buffer: vec![0u8; config.max_packet_size as usize],
            sendtime: boxed_array(0),
            packet_state: boxed_array(pktsend_tp::snd_init),
            frame_idx: boxed_array(0),
            frame_pktlost: boxed_array(0),
            frame_pktsent: boxed_array(0),
            next_send,
            comp_recv: 0,
            sequence_number: 0,
            inflight_packets: 0,
            lost_packets_state: 0,
            frame_number: 0,
            frame_inflight: 0,
            sent_frames: 0,
            received_frames: 0,
            lost_frames: 0,
            frame_timer: 0,
            fps: config.fps,
            frame_interval_us,
            pending_frame: None,
        })
    }

    /// Current application-facing pacing and congestion guidance for video.
    pub fn advice(&mut self) -> PragueVideoRateAdvice {
        self.cc.video_advice()
    }

    /// Current recommended sender bitrate in bits per second.
    pub fn recommended_bitrate_bits_per_sec(&mut self) -> u64 {
        self.advice().pacing_rate_bits_per_sec()
    }

    /// Configured sender bitrate cap in bytes per second.
    pub fn max_configured_bitrate_bytes_per_sec(&self) -> rate_tp {
        self.cc.GetStatePtr().m_max_rate
    }

    /// Configured sender bitrate cap in bits per second.
    pub fn max_configured_bitrate_bits_per_sec(&self) -> u64 {
        self.max_configured_bitrate_bytes_per_sec()
            .saturating_mul(8)
    }

    /// Current in-flight packet count.
    pub fn inflight_packets(&self) -> count_tp {
        self.inflight_packets
    }

    /// Current in-flight frame count.
    pub fn inflight_frames(&self) -> count_tp {
        self.frame_inflight
    }

    /// Whether a frame is currently queued for transmission.
    pub fn has_pending_frame(&self) -> bool {
        self.pending_frame.is_some()
    }

    /// Whether the next frame slot is currently open and no frame is queued.
    pub fn can_queue_frame_now(&mut self) -> bool {
        if self.pending_frame.is_some() {
            return false;
        }
        let now = self.cc.Now();
        self.next_send.wrapping_sub(now) <= 0
    }

    /// Remaining delay until the next frame slot opens, in microseconds.
    pub fn next_frame_delay_us(&mut self) -> time_tp {
        let now = self.cc.Now();
        let delay = self.next_send.wrapping_sub(now);
        if delay > 0 {
            delay
        } else {
            0
        }
    }

    fn advance_frame_slot(&mut self, now: time_tp) {
        if self.frame_timer == 0 {
            self.frame_number = self.frame_number.wrapping_add(1);
            self.frame_timer = now.wrapping_add(self.frame_interval_us);
        } else {
            let mut frame_adv: count_tp = 1;
            if self.frame_timer.wrapping_sub(now) <= 0 {
                frame_adv = 1
                    + ((now.wrapping_sub(self.frame_timer) as i64) * (self.fps as i64) / 1_000_000)
                        as i32;
            }
            self.frame_number = self.frame_number.wrapping_add(frame_adv);
            self.frame_timer = self
                .frame_timer
                .wrapping_add(((frame_adv as i64) * (self.frame_interval_us as i64)) as i32);
        }
        self.comp_recv = 0;
    }

    /// Queue one encoded frame for paced fragmentation and transmission.
    pub fn queue_frame(
        &mut self,
        frame_data: &[u8],
    ) -> Result<PragueQueuedVideoFrame, SessionError> {
        self.queue_frame_owned(frame_data.to_vec())
    }

    /// Queue one owned encoded frame for paced fragmentation and transmission.
    ///
    /// This lets callers hand frame storage to the session without an extra
    /// copy, which is preferable on high-rate RT paths.
    pub fn queue_frame_owned(
        &mut self,
        frame_data: Vec<u8>,
    ) -> Result<PragueQueuedVideoFrame, SessionError> {
        if frame_data.is_empty() {
            return Err(SessionError::InvalidPacket("frame data must not be empty"));
        }
        if self.pending_frame.is_some() {
            return Err(SessionError::FrameAlreadyQueued);
        }

        let now = self.cc.Now();
        let next_frame_in_us = self.next_send.wrapping_sub(now);
        if next_frame_in_us > 0 {
            return Err(SessionError::FrameNotReady { next_frame_in_us });
        }

        self.advance_frame_slot(now);
        let advice = self.advice();
        let queued = PragueQueuedVideoFrame {
            frame_number: self.frame_number,
            actual_frame_size_bytes: frame_data.len() as size_tp,
            target_frame_size_bytes: advice.target_frame_size_bytes,
            over_target: (frame_data.len() as u64) > advice.target_frame_size_bytes,
            advice,
        };

        let frame_idx = idx_frm(self.frame_number);
        self.frame_pktsent[frame_idx] = 0;
        self.frame_pktlost[frame_idx] = 0;
        self.pending_frame = Some(PendingVideoFrame {
            frame_number: self.frame_number,
            frame_data,
            sent_app_bytes: 0,
            advice,
        });
        self.frame_inflight =
            bool_as_count(true) + self.sent_frames - self.received_frames - self.lost_frames;

        Ok(queued)
    }

    /// Send as many queued frame fragments as are currently allowed.
    ///
    /// Returns `Ok(None)` if no frame is queued.
    pub fn transmit_ready_frame_fragments(
        &mut self,
    ) -> Result<Option<PragueVideoSendReport>, SessionError> {
        let (
            frame_number,
            frame_window,
            packet_burst,
            packet_size_bytes,
            pacing_rate_bytes_per_sec,
        ) = match self.pending_frame.as_ref() {
            Some(pending) => (
                pending.frame_number,
                pending.advice.frame_window,
                pending.advice.transport.packet_burst,
                pending.advice.transport.packet_size_bytes,
                pending.advice.transport.pacing_rate_bytes_per_sec,
            ),
            None => return Ok(None),
        };

        if packet_size_bytes <= FrameMessage::SIZE as u64 {
            return Err(SessionError::InvalidPacket("video packet size too small"));
        }

        let now0 = self.cc.Now();
        let next_send_in_us = self.next_send.wrapping_sub(now0);
        if self.frame_inflight > frame_window || next_send_in_us > 0 {
            return Err(SessionError::WouldBlock {
                next_send_in_us: if next_send_in_us > 0 {
                    next_send_in_us
                } else {
                    0
                },
                inflight_packets: self.frame_inflight,
                packet_window: frame_window,
            });
        }

        let mut now = now0;
        let mut inburst: count_tp = 0;
        let mut start_send: time_tp = 0;
        let mut fragments_sent: u32 = 0;
        let mut bytes_sent_on_wire: size_tp = 0;
        let mut app_bytes_sent: size_tp = 0;
        let mut last_sequence_number = self.sequence_number;

        while self.frame_inflight <= frame_window
            && inburst < packet_burst
            && self.next_send.wrapping_sub(now) <= 0
        {
            let (app_offset, fragment_payload_len, total_frame_size) = {
                let pending = self.pending_frame.as_ref().expect("pending frame");
                if pending.sent_app_bytes >= pending.frame_data.len() {
                    break;
                }
                let payload_budget = packet_size_bytes as usize - FrameMessage::SIZE;
                let app_offset = pending.sent_app_bytes;
                let fragment_payload_len =
                    payload_budget.min(pending.frame_data.len() - app_offset);
                let total_frame_size = pending.frame_data.len();
                (app_offset, fragment_payload_len, total_frame_size)
            };

            let send_len = FrameMessage::SIZE + fragment_payload_len;
            if self.send_buffer.len() < send_len {
                self.send_buffer.resize(send_len, 0);
            }
            let (mut timestamp, mut echoed_timestamp, mut next_send_ecn) =
                (0, 0, ecn_tp::ecn_not_ect);
            self.cc
                .GetTimeInfo(&mut timestamp, &mut echoed_timestamp, &mut next_send_ecn);

            if start_send == 0 {
                start_send = now;
            }

            self.sequence_number = self.sequence_number.wrapping_add(1);
            {
                let (pending_frame, send_buffer) = (&mut self.pending_frame, &mut self.send_buffer);
                let pending = pending_frame.as_mut().expect("pending frame");
                let packet = &mut send_buffer[..send_len];
                crate::protocol::pkt_format::encode_frame_message_network(
                    packet,
                    timestamp,
                    echoed_timestamp,
                    self.sequence_number,
                    frame_number,
                    app_offset as count_tp,
                    total_frame_size as count_tp,
                )?;
                packet[FrameMessage::SIZE..send_len].copy_from_slice(
                    &pending.frame_data[app_offset..app_offset + fragment_payload_len],
                );
                pending.sent_app_bytes += fragment_payload_len;
            }
            self.socket.Send(
                &self.send_buffer[..send_len],
                send_len as size_tp,
                next_send_ecn,
            )?;

            let send_idx = (self.sequence_number as u32 % PKT_BUFFER_SIZE as u32) as usize;
            self.sendtime[send_idx] = start_send;
            self.packet_state[send_idx] = pktsend_tp::snd_sent;
            self.frame_idx[send_idx] = frame_number;
            self.frame_pktsent[idx_frm(frame_number)] =
                self.frame_pktsent[idx_frm(frame_number)].wrapping_add(1);
            self.inflight_packets = self.inflight_packets.wrapping_add(1);

            inburst = inburst.wrapping_add(1);
            fragments_sent = fragments_sent.wrapping_add(1);
            bytes_sent_on_wire = bytes_sent_on_wire.wrapping_add(send_len as size_tp);
            app_bytes_sent = app_bytes_sent.wrapping_add(fragment_payload_len as size_tp);
            last_sequence_number = self.sequence_number;
            now = self.cc.Now();
        }

        if fragments_sent == 0 {
            return Err(SessionError::WouldBlock {
                next_send_in_us: self.next_frame_delay_us(),
                inflight_packets: self.frame_inflight,
                packet_window: frame_window,
            });
        }

        let frame_complete = self
            .pending_frame
            .as_ref()
            .is_some_and(|pending| pending.sent_app_bytes >= pending.frame_data.len());
        let remaining_app_bytes = self.pending_frame.as_ref().map_or(0, |pending| {
            (pending.frame_data.len() - pending.sent_app_bytes) as size_tp
        });

        if frame_complete {
            self.next_send = self.frame_timer;
            self.sent_frames = self.sent_frames.wrapping_add(1);
            if self.frame_pktlost[idx_frm(frame_number)] != 0 {
                self.lost_frames = self.lost_frames.wrapping_add(1);
            }
            self.pending_frame = None;
        } else {
            let delta = self.comp_recv.wrapping_add(
                ((bytes_sent_on_wire as i64) * 1_000_000 / pacing_rate_bytes_per_sec.max(1) as i64)
                    as i32,
            );
            self.next_send = start_send.wrapping_add(if delta <= 0 { 1 } else { delta });
            self.comp_recv = 0;
        }

        self.frame_inflight = bool_as_count(self.pending_frame.is_some()) + self.sent_frames
            - self.received_frames
            - self.lost_frames;
        let advice = self.advice();

        Ok(Some(PragueVideoSendReport {
            frame_number,
            fragments_sent,
            bytes_sent_on_wire,
            app_bytes_sent,
            remaining_app_bytes,
            last_sequence_number,
            frame_complete,
            advice,
        }))
    }

    /// Process one incoming classic ACK and update the sender-side Prague frame state.
    ///
    /// Returns `Ok(None)` on timeout.
    pub fn receive_feedback(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueVideoAckFeedback>, SessionError> {
        let mut recv_ecn = ecn_tp::ecn_not_ect;
        let bytes_received =
            self.socket
                .Receive(&mut self.receive_buffer[..], &mut recv_ecn, timeout)?;
        if bytes_received == 0 {
            return Ok(None);
        }

        let bytes_received_usize = bytes_received as usize;
        if self.receive_buffer[0] != PKT_ACK_TYPE || bytes_received_usize < AckMessage::SIZE {
            return Err(SessionError::UnexpectedPacketType(self.receive_buffer[0]));
        }

        let is_sending = self.pending_frame.is_some();
        let (
            acked_sequence_number,
            timestamp,
            echoed_timestamp,
            packets_received,
            packets_ce,
            packets_lost,
            error_l4s,
        ) = {
            let mut ack = AckMessage::new(&mut self.receive_buffer[..bytes_received_usize])?;
            ack.get_frame_stat(
                &mut self.packet_state,
                &mut self.lost_packets_state,
                is_sending,
                self.frame_number,
                &mut self.received_frames,
                &mut self.lost_frames,
                &self.frame_idx,
                &mut self.frame_pktsent,
                &mut self.frame_pktlost,
            );
            (
                ack.ack_seq(),
                ack.timestamp(),
                ack.echoed_timestamp(),
                ack.packets_received(),
                ack.packets_CE(),
                ack.packets_lost(),
                ack.error_L4S(),
            )
        };

        self.frame_inflight = bool_as_count(self.pending_frame.is_some()) + self.sent_frames
            - self.received_frames
            - self.lost_frames;
        self.cc.PacketReceived(timestamp, echoed_timestamp);
        self.cc.ACKReceived(
            packets_received,
            packets_ce,
            packets_lost,
            self.sequence_number,
            error_l4s,
            &mut self.inflight_packets,
        );

        let advice = self.advice();
        Ok(Some(PragueVideoAckFeedback {
            acked_sequence_number,
            bytes_received,
            packets_received,
            packets_ce,
            packets_lost,
            error_l4s,
            inflight_packets: self.inflight_packets,
            inflight_frames: self.frame_inflight,
            sent_frames: self.sent_frames,
            received_frames: self.received_frames,
            lost_frames: self.lost_frames,
            advice,
        }))
    }

    /// Reset the sender-side Prague frame state after a prolonged feedback timeout.
    pub fn on_feedback_timeout(&mut self) -> PragueVideoRateAdvice {
        self.cc.ResetCCInfo();
        self.frame_inflight = 0;
        self.inflight_packets = 0;
        self.next_send = self.cc.Now();
        self.comp_recv = 0;
        self.frame_timer = 0;
        self.pending_frame = None;
        self.advice()
    }
}

impl PragueVideoReceiverSession {
    /// Bind a video/frame receiver session to a local address.
    pub fn bind(addr: &str, port: u16) -> Result<Self, SessionError> {
        Self::bind_with_limits(addr, port, PragueReceiverReassemblyLimits::default())
    }

    /// Bind a video/frame receiver session with explicit reassembly limits.
    pub fn bind_with_limits(
        addr: &str,
        port: u16,
        limits: PragueReceiverReassemblyLimits,
    ) -> Result<Self, SessionError> {
        let limits = limits.validate()?;
        Ok(Self {
            inner: PragueReceiverSession::bind(addr, port)?,
            pending_frames: BTreeMap::new(),
            reassembly_limits: limits,
            next_pending_order: 0,
        })
    }

    /// Current receiver-side congestion view.
    pub fn advice(&mut self) -> PragueRateAdvice {
        self.inner.advice()
    }

    fn pending_frame_bytes(&self) -> usize {
        self.pending_frames
            .values()
            .map(PendingVideoFramePayload::retained_bytes)
            .sum()
    }

    fn evict_oldest_pending_frame_except(&mut self, keep_frame_number: Option<count_tp>) -> bool {
        let Some(frame_number) = self
            .pending_frames
            .iter()
            .filter(|(frame_number, _)| keep_frame_number != Some(**frame_number))
            .min_by_key(|(_, pending)| pending.last_update_order)
            .map(|(frame_number, _)| *frame_number)
        else {
            return false;
        };
        self.pending_frames.remove(&frame_number);
        true
    }

    fn evict_stale_pending_frames(&mut self, now: time_tp, keep_frame_number: Option<count_tp>) {
        let max_age_us = self.reassembly_limits.max_pending_frame_age_us;
        let stale_frame_numbers: Vec<count_tp> = self
            .pending_frames
            .iter()
            .filter(|(frame_number, pending)| {
                keep_frame_number != Some(**frame_number)
                    && now.wrapping_sub(pending.last_update_time) > max_age_us
            })
            .map(|(frame_number, _)| *frame_number)
            .collect();

        for frame_number in stale_frame_numbers {
            self.pending_frames.remove(&frame_number);
        }
    }

    fn prune_pending_frames(
        &mut self,
        now: time_tp,
        keep_frame_number: Option<count_tp>,
        additional_bytes: usize,
    ) {
        self.evict_stale_pending_frames(now, keep_frame_number);

        if !keep_frame_number
            .is_some_and(|frame_number| self.pending_frames.contains_key(&frame_number))
        {
            while self.pending_frames.len() >= self.reassembly_limits.max_pending_frames {
                if !self.evict_oldest_pending_frame_except(keep_frame_number) {
                    break;
                }
            }
        }

        while additional_bytes > 0
            && self.pending_frame_bytes().saturating_add(additional_bytes)
                > self.reassembly_limits.max_pending_frame_bytes
        {
            if !self.evict_oldest_pending_frame_except(keep_frame_number) {
                break;
            }
        }
    }

    /// Receive RT frame fragments, ACK each one, and return a full frame when complete.
    ///
    /// If the timeout expires before the next packet arrives, returns `Ok(None)` and keeps any
    /// partially reassembled frames for a future call.
    pub fn receive_frame_and_ack(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedVideoFrame>, SessionError> {
        loop {
            let received = match self.inner.receive_and_ack_borrowed(timeout)? {
                Some(received) => received,
                None => {
                    let now = self.inner.now();
                    self.prune_pending_frames(now, None, 0);
                    return Ok(None);
                }
            };

            let (frame_number, total_len, offset, chunk) = {
                let packet = match received.packet {
                    PragueReceivedPacketView::Frame(packet) => packet,
                    PragueReceivedPacketView::Bulk(_) => {
                        return Err(SessionError::InvalidPacket(
                            "video receiver does not accept bulk packets",
                        ))
                    }
                };

                let total_len = usize::try_from(packet.frame_size_bytes).map_err(|_| {
                    SessionError::InvalidPacket("RT frame size exceeds platform usize range")
                })?;
                let offset = usize::try_from(packet.frame_offset_bytes).map_err(|_| {
                    SessionError::InvalidPacket("RT frame offset exceeds platform usize range")
                })?;

                (
                    packet.frame_number,
                    total_len,
                    offset,
                    packet.app_data.to_vec(),
                )
            };

            let order = self.next_pending_order;
            self.next_pending_order = self.next_pending_order.wrapping_add(1);
            let now = self.inner.now();
            let additional_bytes = match self.pending_frames.get(&frame_number) {
                Some(pending) => {
                    pending.validate_total_len(total_len)?;
                    pending.additional_bytes_for_chunk(offset, &chunk)?
                }
                None => chunk.len(),
            };
            self.prune_pending_frames(now, Some(frame_number), additional_bytes);
            let mut completed_frame = None;
            {
                let pending = self
                    .pending_frames
                    .entry(frame_number)
                    .or_insert_with(|| PendingVideoFramePayload::new(total_len, order, now));
                pending.validate_total_len(total_len)?;
                pending.insert_chunk_owned(offset, chunk)?;
                pending.last_update_order = order;
                pending.last_update_time = now;
                if pending.is_complete() {
                    completed_frame = self.pending_frames.remove(&frame_number);
                }
            }

            if let Some(frame) = completed_frame {
                return Ok(Some(PragueReceivedVideoFrame {
                    frame_number,
                    frame_size_bytes: frame.total_len as size_tp,
                    payload: frame.into_payload()?,
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_frame_requires_open_frame_slot() {
        let mut session =
            PragueVideoSenderSession::connect("127.0.0.1", 9, PragueVideoSessionConfig::default())
                .expect("video sender session");
        session.next_send = session.cc.Now().wrapping_add(10_000);

        let err = session
            .queue_frame(b"frame")
            .expect_err("frame should not be ready");
        match err {
            SessionError::FrameNotReady { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn pending_video_frame_reassembles_contiguous_payload() {
        let mut pending = PendingVideoFramePayload::new(6, 0, 0);
        pending.insert_chunk(3, b"def").expect("insert second half");
        pending.insert_chunk(0, b"abc").expect("insert first half");

        assert!(pending.is_complete());
        assert_eq!(pending.into_payload().expect("payload"), b"abcdef");
    }

    #[test]
    fn video_receiver_evicts_oldest_incomplete_frame_when_limit_is_hit() {
        let mut receiver = PragueVideoReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_frames: 1,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("video receiver");

        receiver
            .pending_frames
            .insert(1, PendingVideoFramePayload::new(64, 0, 1));
        receiver.prune_pending_frames(1, Some(2), 1);

        assert!(!receiver.pending_frames.contains_key(&1));
        assert!(receiver.pending_frames.is_empty());
    }

    #[test]
    fn video_receiver_evicts_stale_incomplete_frame() {
        let mut receiver = PragueVideoReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_frame_age_us: 10,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("video receiver");

        receiver
            .pending_frames
            .insert(1, PendingVideoFramePayload::new(64, 0, 5));
        receiver.prune_pending_frames(16, None, 0);

        assert!(receiver.pending_frames.is_empty());
    }

    #[test]
    fn video_receiver_evicts_oldest_frame_when_byte_budget_is_hit() {
        let mut receiver = PragueVideoReceiverSession::bind_with_limits(
            "0.0.0.0",
            0,
            PragueReceiverReassemblyLimits {
                max_pending_frame_bytes: 7,
                ..PragueReceiverReassemblyLimits::default()
            },
        )
        .expect("video receiver");

        let mut older = PendingVideoFramePayload::new(16, 0, 1);
        older.insert_chunk(0, b"abcd").expect("older chunk");
        receiver.pending_frames.insert(1, older);

        let mut newer = PendingVideoFramePayload::new(16, 1, 2);
        newer.insert_chunk(0, b"efg").expect("newer chunk");
        receiver.pending_frames.insert(2, newer);

        receiver.prune_pending_frames(2, Some(2), 1);

        assert!(!receiver.pending_frames.contains_key(&1));
        assert!(receiver.pending_frames.contains_key(&2));
        assert_eq!(receiver.pending_frame_bytes(), 3);
    }
}
