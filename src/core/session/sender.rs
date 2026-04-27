use crate::congestion::{
    count_tp, ecn_tp, rate_tp, size_tp, time_tp, PragueCC, PragueRateAdvice, PRAGUE_INITRATE,
    PRAGUE_INITWIN, PRAGUE_MINRATE,
};
use crate::core::SessionError;
use crate::net::UDPSocket;
use crate::protocol::pkt_format::{
    encode_data_message_network, pktsend_tp, AckMessage, DataMessage, BUFFER_SIZE, PKT_ACK_TYPE,
    PKT_BUFFER_SIZE,
};

use super::types::{
    PragueAckFeedback, PragueBulkTransferReport, PragueSendReport, PragueSessionConfig,
};
use super::{boxed_array, sleep_delay_us};

/// Sender-side wrapper for Prague bulk/message traffic.
pub struct PragueSenderSession {
    socket: UDPSocket,
    cc: PragueCC,
    receive_buffer: Vec<u8>,
    send_buffer: Vec<u8>,
    sendtime: Box<[time_tp; PKT_BUFFER_SIZE]>,
    packet_state: Box<[pktsend_tp; PKT_BUFFER_SIZE]>,
    next_send: time_tp,
    sequence_number: count_tp,
    inflight_packets: count_tp,
    lost_packets_state: count_tp,
}

impl PragueSenderSession {
    /// Open a connected sender session to a peer.
    pub fn connect(
        addr: &str,
        port: u16,
        config: PragueSessionConfig,
    ) -> Result<Self, SessionError> {
        let mut socket = UDPSocket::new();
        socket.Connect(addr, port)?;

        let mut cc = PragueCC::new(
            config.max_packet_size,
            0,
            0,
            PRAGUE_INITRATE,
            PRAGUE_INITWIN,
            PRAGUE_MINRATE,
            config.max_rate,
        );
        let next_send = cc.Now();

        Ok(Self {
            socket,
            cc,
            receive_buffer: vec![0u8; BUFFER_SIZE.max(config.max_packet_size as usize)],
            send_buffer: vec![0u8; config.max_packet_size as usize],
            sendtime: boxed_array(0),
            packet_state: boxed_array(pktsend_tp::snd_init),
            next_send,
            sequence_number: 0,
            inflight_packets: 0,
            lost_packets_state: 0,
        })
    }

    pub(super) fn send_bulk_parts(
        &mut self,
        app_prefix: &[u8],
        app_data: &[u8],
    ) -> Result<PragueSendReport, SessionError> {
        let advice = self.advice();
        let now = self.cc.Now();
        let next_send_in_us = self.next_send.wrapping_sub(now);
        if self.inflight_packets >= advice.packet_window || next_send_in_us > 0 {
            return Err(SessionError::WouldBlock {
                next_send_in_us: if next_send_in_us > 0 {
                    next_send_in_us
                } else {
                    0
                },
                inflight_packets: self.inflight_packets,
                packet_window: advice.packet_window,
            });
        }

        let app_len = app_prefix.len().saturating_add(app_data.len());
        let max_payload_len = advice
            .packet_size_bytes
            .saturating_sub(DataMessage::SIZE as u64) as usize;
        if app_len > max_payload_len {
            return Err(SessionError::PayloadTooLarge {
                payload_len: app_len,
                max_payload_len,
            });
        }

        let total_len = DataMessage::SIZE + app_len;
        if self.send_buffer.len() < total_len {
            self.send_buffer.resize(total_len, 0);
        }

        let (mut timestamp, mut echoed_timestamp, mut next_send_ecn) = (0, 0, ecn_tp::ecn_not_ect);
        self.cc
            .GetTimeInfo(&mut timestamp, &mut echoed_timestamp, &mut next_send_ecn);

        self.sequence_number = self.sequence_number.wrapping_add(1);
        {
            let packet = &mut self.send_buffer[..total_len];
            encode_data_message_network(packet, timestamp, echoed_timestamp, self.sequence_number)?;
            let payload_start = DataMessage::SIZE;
            let prefix_end = payload_start + app_prefix.len();
            if !app_prefix.is_empty() {
                packet[payload_start..prefix_end].copy_from_slice(app_prefix);
            }
            if !app_data.is_empty() {
                packet[prefix_end..prefix_end + app_data.len()].copy_from_slice(app_data);
            }
        }

        self.socket.Send(
            &self.send_buffer[..total_len],
            total_len as size_tp,
            next_send_ecn,
        )?;

        let send_idx = (self.sequence_number as u32 % PKT_BUFFER_SIZE as u32) as usize;
        self.sendtime[send_idx] = now;
        self.packet_state[send_idx] = pktsend_tp::snd_sent;
        self.inflight_packets = self.inflight_packets.wrapping_add(1);

        let delta = ((advice.packet_size_bytes as i64) * 1_000_000
            / advice.pacing_rate_bytes_per_sec.max(1) as i64) as i32;
        self.next_send = now.wrapping_add(if delta <= 0 { 1 } else { delta });

        Ok(PragueSendReport {
            sequence_number: self.sequence_number,
            total_bytes: total_len as size_tp,
            app_data_len: app_len,
            advice,
        })
    }

    /// Current application-facing pacing and congestion guidance.
    pub fn advice(&mut self) -> PragueRateAdvice {
        self.cc.bulk_advice()
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

    /// Maximum app bytes that fit after the Prague bulk header right now.
    pub fn max_app_data_len(&mut self) -> usize {
        self.advice()
            .packet_size_bytes
            .saturating_sub(DataMessage::SIZE as u64) as usize
    }

    /// Whether the session can send a new application datagram immediately.
    pub fn can_send_now(&mut self) -> bool {
        let advice = self.advice();
        let now = self.cc.Now();
        self.inflight_packets < advice.packet_window && self.next_send.wrapping_sub(now) <= 0
    }

    /// Remaining delay until the next pacing opportunity, in microseconds.
    pub fn next_send_delay_us(&mut self) -> time_tp {
        let now = self.cc.Now();
        let delay = self.next_send.wrapping_sub(now);
        if delay > 0 {
            delay
        } else {
            0
        }
    }

    /// Send one Prague bulk packet carrying arbitrary application bytes.
    pub fn send_bulk(&mut self, app_data: &[u8]) -> Result<PragueSendReport, SessionError> {
        self.send_bulk_parts(&[], app_data)
    }

    /// Send a large payload by splitting it across multiple Prague bulk packets.
    ///
    /// This helper preserves the existing Prague wire format: it emits ordinary
    /// bulk `DataMessage` packets back-to-back and processes feedback internally
    /// until the whole payload has been sent and all outstanding packets have
    /// been acknowledged. It does not add segment headers or receiver-side
    /// reassembly; applications still own content boundaries above Prague.
    pub fn send_large_bulk_blocking(
        &mut self,
        app_data: &[u8],
        feedback_timeout_us: time_tp,
    ) -> Result<PragueBulkTransferReport, SessionError> {
        if feedback_timeout_us <= 0 {
            return Err(SessionError::InvalidPacket(
                "feedback timeout must be > 0 for blocking bulk transfer",
            ));
        }

        let mut offset = 0usize;
        let mut report = PragueBulkTransferReport {
            packets_sent: 0,
            app_bytes_sent: 0,
            bytes_sent_on_wire: 0,
            last_sequence_number: None,
            feedback_packets_processed: 0,
            inflight_packets: self.inflight_packets,
            advice: self.advice(),
        };

        while offset < app_data.len() || self.inflight_packets > 0 {
            while offset < app_data.len() && self.can_send_now() {
                let max_payload_len = self.max_app_data_len();
                if max_payload_len == 0 {
                    return Err(SessionError::InvalidPacket("bulk packet size too small"));
                }

                let next_offset = (offset + max_payload_len).min(app_data.len());
                let sent = self.send_bulk(&app_data[offset..next_offset])?;
                offset = next_offset;

                report.packets_sent = report.packets_sent.wrapping_add(1);
                report.app_bytes_sent = report
                    .app_bytes_sent
                    .wrapping_add(sent.app_data_len as size_tp);
                report.bytes_sent_on_wire =
                    report.bytes_sent_on_wire.wrapping_add(sent.total_bytes);
                report.last_sequence_number = Some(sent.sequence_number);
                report.inflight_packets = self.inflight_packets;
                report.advice = sent.advice;
            }

            if offset >= app_data.len() && self.inflight_packets == 0 {
                break;
            }

            if offset < app_data.len() && self.inflight_packets == 0 {
                sleep_delay_us(self.next_send_delay_us());
                continue;
            }

            match self.receive_feedback(feedback_timeout_us)? {
                Some(feedback) => {
                    report.feedback_packets_processed =
                        report.feedback_packets_processed.wrapping_add(1);
                    report.inflight_packets = feedback.inflight_packets;
                    report.advice = feedback.advice;
                }
                None => {
                    if offset < app_data.len() && self.can_send_now() {
                        continue;
                    }
                    if offset >= app_data.len() && self.inflight_packets == 0 {
                        break;
                    }
                    return Err(SessionError::FeedbackTimeout {
                        waited_us: feedback_timeout_us,
                        inflight_packets: self.inflight_packets,
                    });
                }
            }
        }

        report.inflight_packets = self.inflight_packets;
        report.advice = self.advice();
        Ok(report)
    }

    /// Process one incoming classic ACK and update the sender-side Prague state.
    ///
    /// Returns `Ok(None)` on timeout.
    pub fn receive_feedback(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueAckFeedback>, SessionError> {
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

        let (acked_sequence_number, packets_received, packets_ce, packets_lost, error_l4s) = {
            let mut ack = AckMessage::new(&mut self.receive_buffer[..bytes_received_usize])?;
            ack.get_stat(&mut self.packet_state, &mut self.lost_packets_state);
            (
                ack.ack_seq(),
                ack.packets_received(),
                ack.packets_CE(),
                ack.packets_lost(),
                ack.error_L4S(),
            )
        };
        self.cc.PacketReceived(
            {
                let ack = AckMessage::new(&mut self.receive_buffer[..bytes_received_usize])?;
                ack.timestamp()
            },
            {
                let ack = AckMessage::new(&mut self.receive_buffer[..bytes_received_usize])?;
                ack.echoed_timestamp()
            },
        );
        self.cc.ACKReceived(
            packets_received,
            packets_ce,
            packets_lost,
            self.sequence_number,
            error_l4s,
            &mut self.inflight_packets,
        );

        let advice = self.advice();
        Ok(Some(PragueAckFeedback {
            acked_sequence_number,
            bytes_received,
            packets_received,
            packets_ce,
            packets_lost,
            error_l4s,
            inflight_packets: self.inflight_packets,
            advice,
        }))
    }

    /// Reset the sender-side Prague state after a prolonged feedback timeout.
    pub fn on_feedback_timeout(&mut self) -> PragueRateAdvice {
        self.cc.ResetCCInfo();
        self.inflight_packets = 0;
        self.next_send = self.cc.Now();
        self.advice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_bulk_reports_would_block_when_window_is_closed() {
        let mut session =
            PragueSenderSession::connect("127.0.0.1", 9, PragueSessionConfig::default())
                .expect("sender session");
        session.inflight_packets = session.advice().packet_window;

        let err = session.send_bulk(b"payload").expect_err("should block");
        match err {
            SessionError::WouldBlock { .. } => {}
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn send_large_bulk_blocking_rejects_non_positive_timeout() {
        let mut session =
            PragueSenderSession::connect("127.0.0.1", 9, PragueSessionConfig::default())
                .expect("sender session");

        let err = session
            .send_large_bulk_blocking(b"payload", 0)
            .expect_err("timeout should be validated");
        match err {
            SessionError::InvalidPacket(_) => {}
            other => panic!("unexpected error: {other}"),
        }
    }
}
