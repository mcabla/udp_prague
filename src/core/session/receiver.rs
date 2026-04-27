use crate::congestion::{count_tp, ecn_tp, size_tp, time_tp, PragueCC, PragueRateAdvice};
use crate::core::SessionError;
use crate::net::UDPSocket;
use crate::protocol::pkt_format::{
    decode_data_message_network, decode_frame_message_network, encode_ack_message_network,
    AckMessage, DataMessage, FrameMessage, BUFFER_SIZE, BULK_DATA_TYPE, RT_DATA_TYPE,
};

use super::types::{
    PragueAckReport, PragueReceivedBulkPacketView, PragueReceivedFramePacketView,
    PragueReceivedPacket, PragueReceivedPacketAndAck, PragueReceivedPacketAndAckView,
    PragueReceivedPacketView,
};

/// Receiver-side wrapper for Prague bulk and frame traffic.
pub struct PragueReceiverSession {
    socket: UDPSocket,
    cc: PragueCC,
    receive_buffer: Vec<u8>,
    ack_buffer: [u8; AckMessage::SIZE],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReceivedPacketMeta {
    Bulk {
        sequence_number: count_tp,
        timestamp: time_tp,
        echoed_timestamp: time_tp,
        ecn: ecn_tp,
        app_data_start: usize,
        app_data_len: usize,
    },
    Frame {
        sequence_number: count_tp,
        timestamp: time_tp,
        echoed_timestamp: time_tp,
        frame_number: count_tp,
        frame_offset_bytes: count_tp,
        frame_size_bytes: count_tp,
        ecn: ecn_tp,
        app_data_start: usize,
        app_data_len: usize,
    },
}

impl ReceivedPacketMeta {
    fn sequence_number(&self) -> count_tp {
        match self {
            ReceivedPacketMeta::Bulk {
                sequence_number, ..
            }
            | ReceivedPacketMeta::Frame {
                sequence_number, ..
            } => *sequence_number,
        }
    }

    fn packet_view<'a>(&self, receive_buffer: &'a [u8]) -> PragueReceivedPacketView<'a> {
        match self {
            ReceivedPacketMeta::Bulk {
                sequence_number,
                timestamp,
                echoed_timestamp,
                ecn,
                app_data_start,
                app_data_len,
            } => PragueReceivedPacketView::Bulk(PragueReceivedBulkPacketView {
                sequence_number: *sequence_number,
                timestamp: *timestamp,
                echoed_timestamp: *echoed_timestamp,
                ecn: *ecn,
                app_data: &receive_buffer[*app_data_start..*app_data_start + *app_data_len],
            }),
            ReceivedPacketMeta::Frame {
                sequence_number,
                timestamp,
                echoed_timestamp,
                frame_number,
                frame_offset_bytes,
                frame_size_bytes,
                ecn,
                app_data_start,
                app_data_len,
            } => PragueReceivedPacketView::Frame(PragueReceivedFramePacketView {
                sequence_number: *sequence_number,
                timestamp: *timestamp,
                echoed_timestamp: *echoed_timestamp,
                frame_number: *frame_number,
                frame_offset_bytes: *frame_offset_bytes,
                frame_size_bytes: *frame_size_bytes,
                ecn: *ecn,
                app_data: &receive_buffer[*app_data_start..*app_data_start + *app_data_len],
            }),
        }
    }

    fn packet_owned(&self, receive_buffer: &[u8]) -> PragueReceivedPacket {
        self.packet_view(receive_buffer).to_owned()
    }
}

impl PragueReceiverSession {
    /// Bind a receiver session to a local address.
    pub fn bind(addr: &str, port: u16) -> Result<Self, SessionError> {
        let mut socket = UDPSocket::new();
        socket.Bind(addr, port)?;

        Ok(Self {
            socket,
            cc: PragueCC::default(),
            receive_buffer: vec![0u8; BUFFER_SIZE],
            ack_buffer: [0u8; AckMessage::SIZE],
        })
    }

    /// Current receiver-side congestion view. This can be used to inspect L4S
    /// fallback state or queue pressure seen by the receiver.
    pub fn advice(&mut self) -> PragueRateAdvice {
        self.cc.bulk_advice()
    }

    pub(super) fn now(&mut self) -> time_tp {
        self.cc.Now()
    }

    fn receive_meta(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<ReceivedPacketMeta>, SessionError> {
        let mut recv_ecn = ecn_tp::ecn_not_ect;
        let bytes_received =
            self.socket
                .Receive(&mut self.receive_buffer[..], &mut recv_ecn, timeout)?;
        if bytes_received == 0 {
            return Ok(None);
        }

        let bytes_received = bytes_received as usize;
        match self.receive_buffer[0] {
            BULK_DATA_TYPE => {
                let (timestamp, echoed_timestamp, sequence_number) =
                    decode_data_message_network(&self.receive_buffer[..bytes_received])?;
                self.cc.PacketReceived(timestamp, echoed_timestamp);
                self.cc.DataReceivedSequence(recv_ecn, sequence_number);

                Ok(Some(ReceivedPacketMeta::Bulk {
                    sequence_number,
                    timestamp,
                    echoed_timestamp,
                    ecn: recv_ecn,
                    app_data_start: DataMessage::SIZE,
                    app_data_len: bytes_received.saturating_sub(DataMessage::SIZE),
                }))
            }
            RT_DATA_TYPE => {
                let (
                    timestamp,
                    echoed_timestamp,
                    sequence_number,
                    frame_number,
                    frame_offset_bytes,
                    frame_size_bytes,
                ) = decode_frame_message_network(&self.receive_buffer[..bytes_received])?;
                self.cc.PacketReceived(timestamp, echoed_timestamp);
                self.cc.DataReceivedSequence(recv_ecn, sequence_number);

                let available = bytes_received.saturating_sub(FrameMessage::SIZE);
                let expected = (frame_size_bytes - frame_offset_bytes).max(0) as usize;
                let fragment_len = available.min(expected);

                Ok(Some(ReceivedPacketMeta::Frame {
                    sequence_number,
                    timestamp,
                    echoed_timestamp,
                    frame_number,
                    frame_offset_bytes,
                    frame_size_bytes,
                    ecn: recv_ecn,
                    app_data_start: FrameMessage::SIZE,
                    app_data_len: fragment_len,
                }))
            }
            ty => Err(SessionError::UnexpectedPacketType(ty)),
        }
    }

    /// Receive one Prague packet as a borrowed view into the session receive buffer.
    ///
    /// Returns `Ok(None)` on timeout. The returned packet view borrows the
    /// session buffer and becomes invalid after the next receive call.
    pub fn receive_borrowed(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedPacketView<'_>>, SessionError> {
        let meta = match self.receive_meta(timeout)? {
            Some(meta) => meta,
            None => return Ok(None),
        };
        Ok(Some(meta.packet_view(&self.receive_buffer)))
    }

    /// Receive one Prague data packet and update receiver-side state.
    ///
    /// Returns `Ok(None)` on timeout.
    pub fn receive(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedPacket>, SessionError> {
        let meta = match self.receive_meta(timeout)? {
            Some(meta) => meta,
            None => return Ok(None),
        };
        Ok(Some(meta.packet_owned(&self.receive_buffer)))
    }

    /// Send one classic ACK for a previously received Prague packet.
    pub fn acknowledge(
        &mut self,
        sequence_number: count_tp,
    ) -> Result<PragueAckReport, SessionError> {
        let (mut timestamp, mut echoed_timestamp, mut next_send_ecn) = (0, 0, ecn_tp::ecn_not_ect);
        self.cc
            .GetTimeInfo(&mut timestamp, &mut echoed_timestamp, &mut next_send_ecn);

        let (mut packets_received, mut packets_ce, mut packets_lost, mut error_l4s) =
            (0, 0, 0, false);
        self.cc.GetACKInfo(
            &mut packets_received,
            &mut packets_ce,
            &mut packets_lost,
            &mut error_l4s,
        );

        encode_ack_message_network(
            &mut self.ack_buffer,
            sequence_number,
            timestamp,
            echoed_timestamp,
            packets_received,
            packets_ce,
            packets_lost,
            error_l4s,
        )?;
        self.socket
            .Send(&self.ack_buffer, AckMessage::SIZE as size_tp, next_send_ecn)?;

        Ok(PragueAckReport {
            acked_sequence_number: sequence_number,
            bytes_sent: AckMessage::SIZE as size_tp,
            packets_received,
            packets_ce,
            packets_lost,
            error_l4s,
            next_send_ecn,
        })
    }

    /// Receive one Prague packet view and immediately acknowledge it.
    pub fn receive_and_ack_borrowed(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedPacketAndAckView<'_>>, SessionError> {
        let meta = match self.receive_meta(timeout)? {
            Some(meta) => meta,
            None => return Ok(None),
        };
        let ack = self.acknowledge(meta.sequence_number())?;
        let packet = meta.packet_view(&self.receive_buffer);
        Ok(Some(PragueReceivedPacketAndAckView { packet, ack }))
    }

    /// Receive one Prague packet and immediately acknowledge it.
    pub fn receive_and_ack(
        &mut self,
        timeout: time_tp,
    ) -> Result<Option<PragueReceivedPacketAndAck>, SessionError> {
        let meta = match self.receive_meta(timeout)? {
            Some(meta) => meta,
            None => return Ok(None),
        };
        let ack = self.acknowledge(meta.sequence_number())?;
        let packet = meta.packet_owned(&self.receive_buffer);
        Ok(Some(PragueReceivedPacketAndAck { packet, ack }))
    }
}
