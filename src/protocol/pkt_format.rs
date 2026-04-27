//! Packet wire formats for the UDP Prague example.
//!
//! This module is a near-literal, layout-compatible port of `pkt_format.h`.
//!
//! The reference C++ uses `#pragma pack(push, 1)` and overlays the packet
//! structs directly onto byte buffers, then performs in-place endian
//! conversion using `htonl/htons` (which are involutive). To keep the port
//! interchangeable and easy to compare, we emulate that same behavior by
//! operating on raw byte slices and performing in-place byte swaps on
//! little-endian targets.
//!
//! Important: Callers are expected to fill fields in *host/native* byte order
//! and then call [`DataMessage::hton`], [`FrameMessage::hton`],
//! [`AckMessage::set_stat`], or [`Rfc8888Ack::set_stat`] before sending.
//! On receive, callers call [`DataMessage::hton`], [`AckMessage::get_stat`],
//! or [`Rfc8888Ack::get_stat`] to convert fields back to host order.

use core::fmt;

use crate::congestion::{count_tp, ecn_tp, size_tp, time_tp};

/// Packet payload buffer size in bytes.
///
/// Matches `#define BUFFER_SIZE 8192`.
pub const BUFFER_SIZE: usize = 8192;
/// Maximum MTU size used by the reference project.
pub const MAX_MTU: usize = 9000;

/// Packet buffer size for modulo indexing.
pub const PKT_BUFFER_SIZE: usize = 65536;
/// Frame buffer size (real-time mode).
pub const FRM_BUFFER_SIZE: usize = 2048;
/// RFC8888 report array capacity.
pub const REPORT_SIZE: usize = 2048;

/// Receiver timeout for keeping already-ACKed items in RFC8888 mode.
pub const RCV_TIMEOUT: time_tp = 250_000;
/// Sender timeout for waiting on ACKs.
pub const SND_TIMEOUT: time_tp = 1_000_000;

/// Data packet type (bulk mode).
pub const BULK_DATA_TYPE: u8 = 0x1;
/// Data packet type (real-time mode).
pub const RT_DATA_TYPE: u8 = 0x2;
/// Normal ACK packet type.
pub const PKT_ACK_TYPE: u8 = 0x11;
/// RFC8888 ACK packet type.
pub const RFC8888_ACK_TYPE: u8 = 0x12;

/// Sender-side per-packet state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum pktsend_tp {
    snd_init = 0,
    snd_sent = 1,
    snd_recv = 2,
    snd_lost = 3,
}

/// Receiver-side per-packet state (RFC8888 mode).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum pktrecv_tp {
    rcv_init = 0,
    rcv_recv = 1,
    rcv_ackd = 2,
    rcv_lost = 3,
}

/// Errors returned by packet view constructors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PacketError {
    /// Provided buffer is too small for the requested view.
    BufferTooSmall,
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketError::BufferTooSmall => write!(f, "buffer too small"),
        }
    }
}

impl std::error::Error for PacketError {}

#[inline]
fn idx_pkt(seq: count_tp) -> usize {
    // Match C++ modulo behavior for indexing, even if `seq` wraps the i32 range.
    (seq as u32 % PKT_BUFFER_SIZE as u32) as usize
}

#[inline]
fn idx_frm(frm: count_tp) -> usize {
    (frm as u32 % FRM_BUFFER_SIZE as u32) as usize
}

#[inline]
fn write_u8(buf: &mut [u8], off: usize, v: u8) {
    buf[off] = v;
}

#[inline]
fn read_u8(buf: &[u8], off: usize) -> u8 {
    buf[off]
}

#[inline]
fn write_bool_u8(buf: &mut [u8], off: usize, v: bool) {
    buf[off] = if v { 1 } else { 0 };
}

#[inline]
fn read_bool_u8(buf: &[u8], off: usize) -> bool {
    buf[off] != 0
}

#[inline]
fn write_i32_ne(buf: &mut [u8], off: usize, v: i32) {
    buf[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}

#[inline]
fn write_i32_be(buf: &mut [u8], off: usize, v: i32) {
    buf[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

#[inline]
fn read_i32_ne(buf: &[u8], off: usize) -> i32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&buf[off..off + 4]);
    i32::from_ne_bytes(a)
}

#[inline]
fn read_i32_be(buf: &[u8], off: usize) -> i32 {
    let mut a = [0u8; 4];
    a.copy_from_slice(&buf[off..off + 4]);
    i32::from_be_bytes(a)
}

#[inline]
fn write_u16_ne(buf: &mut [u8], off: usize, v: u16) {
    buf[off..off + 2].copy_from_slice(&v.to_ne_bytes());
}

#[inline]
fn read_u16_ne(buf: &[u8], off: usize) -> u16 {
    let mut a = [0u8; 2];
    a.copy_from_slice(&buf[off..off + 2]);
    u16::from_ne_bytes(a)
}

#[inline]
fn swap2(buf: &mut [u8], off: usize) {
    #[cfg(target_endian = "little")]
    {
        buf.swap(off, off + 1);
    }
}

#[inline]
fn swap4(buf: &mut [u8], off: usize) {
    #[cfg(target_endian = "little")]
    {
        buf.swap(off, off + 3);
        buf.swap(off + 1, off + 2);
    }
}

/// View over a `datamessage_t` packet.
///
/// Layout (packed, 13 bytes):
///
/// ```text
/// 0:  u8   type
/// 1:  i32  timestamp
/// 5:  i32  echoed_timestamp
/// 9:  i32  seq_nr
/// ```
pub struct DataMessage<'a> {
    buf: &'a mut [u8],
}

impl<'a> DataMessage<'a> {
    pub const SIZE: usize = 1 + 4 + 4 + 4;

    pub fn new(buf: &'a mut [u8]) -> Result<Self, PacketError> {
        if buf.len() < Self::SIZE {
            return Err(PacketError::BufferTooSmall);
        }
        Ok(Self { buf })
    }

    #[inline]
    pub fn typ(&self) -> u8 {
        read_u8(self.buf, 0)
    }

    #[inline]
    pub fn timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 1)
    }

    #[inline]
    pub fn echoed_timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 5)
    }

    #[inline]
    pub fn seq_nr(&self) -> count_tp {
        read_i32_ne(self.buf, 9)
    }

    #[inline]
    pub fn set_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 1, v);
    }

    #[inline]
    pub fn set_echoed_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 5, v);
    }

    #[inline]
    pub fn set_seq_nr(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 9, v);
    }

    /// In-place host<->network byte order conversion.
    ///
    /// Matches C++ `datamessage_t::hton()`.
    #[inline]
    pub fn hton(&mut self) {
        write_u8(self.buf, 0, BULK_DATA_TYPE);
        swap4(self.buf, 1);
        swap4(self.buf, 5);
        swap4(self.buf, 9);
    }
}

#[inline]
pub fn encode_data_message_network(
    buf: &mut [u8],
    timestamp: time_tp,
    echoed_timestamp: time_tp,
    seq_nr: count_tp,
) -> Result<(), PacketError> {
    if buf.len() < DataMessage::SIZE {
        return Err(PacketError::BufferTooSmall);
    }

    write_u8(buf, 0, BULK_DATA_TYPE);
    write_i32_be(buf, 1, timestamp);
    write_i32_be(buf, 5, echoed_timestamp);
    write_i32_be(buf, 9, seq_nr);
    Ok(())
}

#[inline]
pub fn decode_data_message_network(
    buf: &[u8],
) -> Result<(time_tp, time_tp, count_tp), PacketError> {
    if buf.len() < DataMessage::SIZE {
        return Err(PacketError::BufferTooSmall);
    }

    Ok((
        read_i32_be(buf, 1),
        read_i32_be(buf, 5),
        read_i32_be(buf, 9),
    ))
}

/// View over a `framemessage_t` packet.
///
/// Layout (packed, 25 bytes):
///
/// ```text
/// 0:  u8   type
/// 1:  i32  timestamp
/// 5:  i32  echoed_timestamp
/// 9:  i32  seq_nr
/// 13: i32  frame_nr
/// 17: i32  frame_sent
/// 21: i32  frame_size
/// ```
pub struct FrameMessage<'a> {
    buf: &'a mut [u8],
}

impl<'a> FrameMessage<'a> {
    pub const SIZE: usize = 1 + 6 * 4;

    pub fn new(buf: &'a mut [u8]) -> Result<Self, PacketError> {
        if buf.len() < Self::SIZE {
            return Err(PacketError::BufferTooSmall);
        }
        Ok(Self { buf })
    }

    #[inline]
    pub fn timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 1)
    }

    #[inline]
    pub fn echoed_timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 5)
    }

    #[inline]
    pub fn set_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 1, v);
    }

    #[inline]
    pub fn set_echoed_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 5, v);
    }

    #[inline]
    pub fn set_seq_nr(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 9, v);
    }

    #[inline]
    pub fn set_frame_nr(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 13, v);
    }

    #[inline]
    pub fn set_frame_sent(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 17, v);
    }

    #[inline]
    pub fn set_frame_size(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 21, v);
    }

    #[inline]
    pub fn seq_nr(&self) -> count_tp {
        read_i32_ne(self.buf, 9)
    }

    #[inline]
    pub fn frame_nr(&self) -> count_tp {
        read_i32_ne(self.buf, 13)
    }

    #[inline]
    pub fn frame_sent(&self) -> count_tp {
        read_i32_ne(self.buf, 17)
    }

    #[inline]
    pub fn frame_size(&self) -> count_tp {
        read_i32_ne(self.buf, 21)
    }

    /// In-place host<->network byte order conversion.
    ///
    /// Matches C++ `framemessage_t::hton()`.
    #[inline]
    pub fn hton(&mut self) {
        write_u8(self.buf, 0, RT_DATA_TYPE);
        swap4(self.buf, 1);
        swap4(self.buf, 5);
        swap4(self.buf, 9);
        swap4(self.buf, 13);
        swap4(self.buf, 17);
        swap4(self.buf, 21);
    }
}

#[inline]
pub fn encode_frame_message_network(
    buf: &mut [u8],
    timestamp: time_tp,
    echoed_timestamp: time_tp,
    seq_nr: count_tp,
    frame_nr: count_tp,
    frame_sent: count_tp,
    frame_size: count_tp,
) -> Result<(), PacketError> {
    if buf.len() < FrameMessage::SIZE {
        return Err(PacketError::BufferTooSmall);
    }

    write_u8(buf, 0, RT_DATA_TYPE);
    write_i32_be(buf, 1, timestamp);
    write_i32_be(buf, 5, echoed_timestamp);
    write_i32_be(buf, 9, seq_nr);
    write_i32_be(buf, 13, frame_nr);
    write_i32_be(buf, 17, frame_sent);
    write_i32_be(buf, 21, frame_size);
    Ok(())
}

#[inline]
pub fn decode_frame_message_network(
    buf: &[u8],
) -> Result<(time_tp, time_tp, count_tp, count_tp, count_tp, count_tp), PacketError> {
    if buf.len() < FrameMessage::SIZE {
        return Err(PacketError::BufferTooSmall);
    }

    Ok((
        read_i32_be(buf, 1),
        read_i32_be(buf, 5),
        read_i32_be(buf, 9),
        read_i32_be(buf, 13),
        read_i32_be(buf, 17),
        read_i32_be(buf, 21),
    ))
}

/// View over a normal `ackmessage_t` packet.
///
/// Layout (packed, 26 bytes):
///
/// ```text
/// 0:  u8   type
/// 1:  i32  ack_seq
/// 5:  i32  timestamp
/// 9:  i32  echoed_timestamp
/// 13: i32  packets_received
/// 17: i32  packets_CE
/// 21: i32  packets_lost
/// 25: u8   error_L4S (bool)
/// ```
pub struct AckMessage<'a> {
    buf: &'a mut [u8],
}

impl<'a> AckMessage<'a> {
    pub const SIZE: usize = 26;

    pub fn new(buf: &'a mut [u8]) -> Result<Self, PacketError> {
        if buf.len() < Self::SIZE {
            return Err(PacketError::BufferTooSmall);
        }
        Ok(Self { buf })
    }

    #[inline]
    pub fn ack_seq(&self) -> count_tp {
        read_i32_ne(self.buf, 1)
    }

    #[inline]
    pub fn timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 5)
    }

    #[inline]
    pub fn echoed_timestamp(&self) -> time_tp {
        read_i32_ne(self.buf, 9)
    }

    #[inline]
    pub fn packets_received(&self) -> count_tp {
        read_i32_ne(self.buf, 13)
    }

    #[inline]
    pub fn packets_CE(&self) -> count_tp {
        read_i32_ne(self.buf, 17)
    }

    #[inline]
    pub fn packets_lost(&self) -> count_tp {
        read_i32_ne(self.buf, 21)
    }

    #[inline]
    pub fn error_L4S(&self) -> bool {
        read_bool_u8(self.buf, 25)
    }

    #[inline]
    pub fn set_ack_seq(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 1, v);
    }

    #[inline]
    pub fn set_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 5, v);
    }

    #[inline]
    pub fn set_echoed_timestamp(&mut self, v: time_tp) {
        write_i32_ne(self.buf, 9, v);
    }

    #[inline]
    pub fn set_packets_received(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 13, v);
    }

    #[inline]
    pub fn set_packets_CE(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 17, v);
    }

    #[inline]
    pub fn set_packets_lost(&mut self, v: count_tp) {
        write_i32_ne(self.buf, 21, v);
    }

    #[inline]
    pub fn set_error_L4S(&mut self, v: bool) {
        write_bool_u8(self.buf, 25, v);
    }

    /// Convert host order fields to network order and set packet type.
    ///
    /// Matches C++ `ackmessage_t::set_stat()`.
    #[inline]
    pub fn set_stat(&mut self) {
        write_u8(self.buf, 0, PKT_ACK_TYPE);
        swap4(self.buf, 1);
        swap4(self.buf, 5);
        swap4(self.buf, 9);
        swap4(self.buf, 13);
        swap4(self.buf, 17);
        swap4(self.buf, 21);
    }

    /// Convert network order fields to host order and update sender-side state.
    ///
    /// Matches C++ `ackmessage_t::get_stat()`.
    pub fn get_stat(
        &mut self,
        pkts_stat: &mut [pktsend_tp; PKT_BUFFER_SIZE],
        packets_lost_state: &mut count_tp,
    ) {
        swap4(self.buf, 1);
        swap4(self.buf, 5);
        swap4(self.buf, 9);
        swap4(self.buf, 13);
        swap4(self.buf, 17);
        swap4(self.buf, 21);

        let ack_seq = self.ack_seq();
        let packets_lost = self.packets_lost();

        pkts_stat[idx_pkt(ack_seq)] = pktsend_tp::snd_recv;

        let diff = packets_lost.wrapping_sub(*packets_lost_state);
        if diff > 0 {
            let diff_u = diff as u32;
            for i in 1..=diff_u {
                let seq_i = ack_seq.wrapping_sub(i as i32);
                let idx = idx_pkt(seq_i);
                if pkts_stat[idx] == pktsend_tp::snd_sent {
                    pkts_stat[idx] = pktsend_tp::snd_lost;
                }
            }
        }
        *packets_lost_state = packets_lost;
    }

    /// Convert network order fields to host order and update sender-side state (real-time mode).
    ///
    /// Matches C++ `ackmessage_t::get_frame_stat()`.
    #[allow(clippy::too_many_arguments)]
    pub fn get_frame_stat(
        &mut self,
        pkts_stat: &mut [pktsend_tp; PKT_BUFFER_SIZE],
        packets_lost_state: &mut count_tp,
        is_sending: bool,
        frm_sending: count_tp,
        recv_frame: &mut count_tp,
        lost_frame: &mut count_tp,
        frm_idx: &[count_tp; PKT_BUFFER_SIZE],
        frm_pktsent: &mut [count_tp; FRM_BUFFER_SIZE],
        frm_pktlost: &mut [count_tp; FRM_BUFFER_SIZE],
    ) {
        swap4(self.buf, 1);
        swap4(self.buf, 5);
        swap4(self.buf, 9);
        swap4(self.buf, 13);
        swap4(self.buf, 17);
        swap4(self.buf, 21);

        let ack_seq = self.ack_seq();
        let packets_lost = self.packets_lost();

        let idx0 = idx_pkt(ack_seq);
        let mut frm_index = frm_idx[idx0];
        let fidx0 = idx_frm(frm_index);

        match pkts_stat[idx0] {
            pktsend_tp::snd_sent => {
                frm_pktsent[fidx0] = frm_pktsent[fidx0].wrapping_sub(1);
                if (frm_index != frm_sending || !is_sending)
                    && frm_pktsent[fidx0] == 0
                    && frm_pktlost[fidx0] == 0
                {
                    *recv_frame = recv_frame.wrapping_add(1);
                }
            }
            pktsend_tp::snd_lost => {
                frm_pktlost[fidx0] = frm_pktlost[fidx0].wrapping_sub(1);
                if (frm_index != frm_sending || !is_sending) && frm_pktlost[fidx0] == 0 {
                    *lost_frame = lost_frame.wrapping_sub(1);
                    if frm_pktsent[fidx0] == 0 {
                        *recv_frame = recv_frame.wrapping_add(1);
                    }
                }
            }
            _ => {}
        }

        pkts_stat[idx0] = pktsend_tp::snd_recv;

        let diff = packets_lost.wrapping_sub(*packets_lost_state);
        if diff > 0 {
            let diff_u = diff as u32;
            for i in 1..=diff_u {
                let seq_i = ack_seq.wrapping_sub(i as i32);
                let idx = idx_pkt(seq_i);
                if pkts_stat[idx] == pktsend_tp::snd_sent {
                    frm_index = frm_idx[idx];
                    let fidx = idx_frm(frm_index);
                    frm_pktsent[fidx] = frm_pktsent[fidx].wrapping_sub(1);
                    if (frm_index != frm_sending || !is_sending) && frm_pktlost[fidx] == 0 {
                        *lost_frame = lost_frame.wrapping_add(1);
                    }
                    frm_pktlost[fidx] = frm_pktlost[fidx].wrapping_add(1);
                    pkts_stat[idx] = pktsend_tp::snd_lost;
                }
            }
        }
        *packets_lost_state = packets_lost;
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
pub fn encode_ack_message_network(
    buf: &mut [u8],
    ack_seq: count_tp,
    timestamp: time_tp,
    echoed_timestamp: time_tp,
    packets_received: count_tp,
    packets_ce: count_tp,
    packets_lost: count_tp,
    error_l4s: bool,
) -> Result<(), PacketError> {
    if buf.len() < AckMessage::SIZE {
        return Err(PacketError::BufferTooSmall);
    }

    write_u8(buf, 0, PKT_ACK_TYPE);
    write_i32_be(buf, 1, ack_seq);
    write_i32_be(buf, 5, timestamp);
    write_i32_be(buf, 9, echoed_timestamp);
    write_i32_be(buf, 13, packets_received);
    write_i32_be(buf, 17, packets_ce);
    write_i32_be(buf, 21, packets_lost);
    write_bool_u8(buf, 25, error_l4s);
    Ok(())
}

/// View over an RFC8888 ACK packet (`rfc8888ack_t`).
///
/// Layout (packed):
///
/// ```text
/// 0:  u8   type
/// 1:  i32  begin_seq
/// 5:  u16  num_reports
/// 7:  u16  report[num_reports]
/// ```
pub struct Rfc8888Ack<'a> {
    buf: &'a mut [u8],
}

impl<'a> Rfc8888Ack<'a> {
    pub const HEADER_SIZE: usize = 1 + 4 + 2;

    pub fn new(buf: &'a mut [u8]) -> Result<Self, PacketError> {
        if buf.len() < Self::HEADER_SIZE {
            return Err(PacketError::BufferTooSmall);
        }
        Ok(Self { buf })
    }

    #[inline]
    pub fn get_size(&self, rptsize: u16) -> u16 {
        (Self::HEADER_SIZE + (rptsize as usize) * 2) as u16
    }

    #[inline]
    pub fn begin_seq(&self) -> count_tp {
        read_i32_ne(self.buf, 1)
    }

    #[inline]
    pub fn num_reports(&self) -> u16 {
        read_u16_ne(self.buf, 5)
    }

    #[inline]
    fn report_off(i: u16) -> usize {
        Self::HEADER_SIZE + (i as usize) * 2
    }

    #[inline]
    fn report(&self, i: u16) -> u16 {
        read_u16_ne(self.buf, Self::report_off(i))
    }

    #[inline]
    fn set_report(&mut self, i: u16, v: u16) {
        write_u16_ne(self.buf, Self::report_off(i), v);
    }

    /// Port of `rfc8888ack_t::get_stat()`.
    ///
    /// Returns number of RTT samples filled into `pkts_rtt`.
    #[allow(clippy::too_many_arguments)]
    pub fn get_stat(
        &mut self,
        now: time_tp,
        sendtime: &[time_tp; PKT_BUFFER_SIZE],
        pkts_rtt: &mut [time_tp; REPORT_SIZE],
        rcvd: &mut count_tp,
        lost: &mut count_tp,
        mark: &mut count_tp,
        error: &mut bool,
        pkts_stat: &mut [pktsend_tp; PKT_BUFFER_SIZE],
        last_ack: &mut count_tp,
    ) -> u16 {
        let mut num_rtt: u16 = 0;

        // swap header into host order
        swap4(self.buf, 1);
        swap2(self.buf, 5);

        let begin_seq = self.begin_seq();
        let num_reports = self.num_reports();

        while last_ack.wrapping_add(1).wrapping_sub(begin_seq) < 0 {
            let s = last_ack.wrapping_add(1);
            let idx = idx_pkt(s);
            if pkts_stat[idx] == pktsend_tp::snd_sent {
                *lost = lost.wrapping_add(1);
                pkts_stat[idx] = pktsend_tp::snd_lost;
            }
            *last_ack = s;
        }

        for i in 0..num_reports {
            let idx = idx_pkt(begin_seq.wrapping_add(i as i32));

            // swap report entry into host order
            let off = Self::report_off(i);
            swap2(self.buf, off);
            let rep = self.report(i);

            if ((rep & 0x8000) >> 15) != 0 {
                if pkts_stat[idx] == pktsend_tp::snd_sent || pkts_stat[idx] == pktsend_tp::snd_lost
                {
                    *rcvd = rcvd.wrapping_add(1);
                    *mark =
                        mark.wrapping_add((((rep & 0x6000) >> 13) == ecn_tp::ecn_ce as u16) as i32);
                    *error |= ((rep & 0x2000) >> 13) == 0x0;
                    pkts_rtt[num_rtt as usize] = now
                        .wrapping_sub(((rep & 0x1FFF) as i32) << 10)
                        .wrapping_sub(sendtime[idx]);
                    num_rtt = num_rtt.wrapping_add(1);
                    if pkts_stat[idx] == pktsend_tp::snd_lost {
                        *lost = lost.wrapping_sub(1);
                    }
                    pkts_stat[idx] = pktsend_tp::snd_recv;
                }
            } else if pkts_stat[idx] == pktsend_tp::snd_sent {
                *lost = lost.wrapping_add(1);
                pkts_stat[idx] = pktsend_tp::snd_lost;
            }

            *last_ack = last_ack.wrapping_add(1);
        }

        num_rtt
    }

    /// Port of `rfc8888ack_t::get_frame_stat()`.
    #[allow(clippy::too_many_arguments)]
    pub fn get_frame_stat(
        &mut self,
        now: time_tp,
        sendtime: &[time_tp; PKT_BUFFER_SIZE],
        pkts_rtt: &mut [time_tp; REPORT_SIZE],
        rcvd: &mut count_tp,
        lost: &mut count_tp,
        mark: &mut count_tp,
        error: &mut bool,
        pkts_stat: &mut [pktsend_tp; PKT_BUFFER_SIZE],
        last_ack: &mut count_tp,
        is_sending: bool,
        frm_sending: count_tp,
        recv_frame: &mut count_tp,
        lost_frame: &mut count_tp,
        frm_idx: &[count_tp; PKT_BUFFER_SIZE],
        frm_pktsent: &mut [count_tp; FRM_BUFFER_SIZE],
        frm_pktlost: &mut [count_tp; FRM_BUFFER_SIZE],
    ) -> u16 {
        let mut num_rtt: u16 = 0;

        swap4(self.buf, 1);
        swap2(self.buf, 5);

        let begin_seq = self.begin_seq();
        let num_reports = self.num_reports();

        while last_ack.wrapping_add(1).wrapping_sub(begin_seq) < 0 {
            let s = last_ack.wrapping_add(1);
            let idx = idx_pkt(s);
            if pkts_stat[idx] == pktsend_tp::snd_sent {
                *lost = lost.wrapping_add(1);
                let frm_index = frm_idx[idx];
                let fidx = idx_frm(frm_index);
                frm_pktsent[fidx] = frm_pktsent[fidx].wrapping_sub(1);
                if (frm_index != frm_sending || !is_sending) && frm_pktlost[fidx] == 0 {
                    *lost_frame = lost_frame.wrapping_add(1);
                }
                frm_pktlost[fidx] = frm_pktlost[fidx].wrapping_add(1);
                pkts_stat[idx] = pktsend_tp::snd_lost;
            }
            *last_ack = s;
        }

        for i in 0..num_reports {
            let idx = idx_pkt(begin_seq.wrapping_add(i as i32));

            let off = Self::report_off(i);
            swap2(self.buf, off);
            let rep = self.report(i);

            if ((rep & 0x8000) >> 15) != 0 {
                if pkts_stat[idx] == pktsend_tp::snd_sent || pkts_stat[idx] == pktsend_tp::snd_lost
                {
                    *rcvd = rcvd.wrapping_add(1);
                    *mark =
                        mark.wrapping_add((((rep & 0x6000) >> 13) == ecn_tp::ecn_ce as u16) as i32);
                    *error |= ((rep & 0x2000) >> 13) == 0x0;
                    pkts_rtt[num_rtt as usize] = now
                        .wrapping_sub(((rep & 0x1FFF) as i32) << 10)
                        .wrapping_sub(sendtime[idx]);
                    num_rtt = num_rtt.wrapping_add(1);
                    if pkts_stat[idx] == pktsend_tp::snd_lost {
                        *lost = lost.wrapping_sub(1);
                    }

                    let frm_index = frm_idx[idx];
                    let fidx = idx_frm(frm_index);

                    if pkts_stat[idx] == pktsend_tp::snd_sent {
                        frm_pktsent[fidx] = frm_pktsent[fidx].wrapping_sub(1);
                        if (frm_index != frm_sending || !is_sending)
                            && frm_pktsent[fidx] == 0
                            && frm_pktlost[fidx] == 0
                        {
                            *recv_frame = recv_frame.wrapping_add(1);
                        }
                    } else if pkts_stat[idx] == pktsend_tp::snd_lost {
                        frm_pktlost[fidx] = frm_pktlost[fidx].wrapping_sub(1);
                        if (frm_index != frm_sending || !is_sending) && frm_pktlost[fidx] == 0 {
                            *lost_frame = lost_frame.wrapping_sub(1);
                            if frm_pktsent[fidx] == 0 {
                                *recv_frame = recv_frame.wrapping_add(1);
                            }
                        }
                    }

                    pkts_stat[idx] = pktsend_tp::snd_recv;
                }
            } else if pkts_stat[idx] == pktsend_tp::snd_sent {
                *lost = lost.wrapping_add(1);
                let frm_index = frm_idx[idx];
                let fidx = idx_frm(frm_index);
                frm_pktsent[fidx] = frm_pktsent[fidx].wrapping_sub(1);
                if (frm_index != frm_sending || !is_sending) && frm_pktlost[fidx] == 0 {
                    *lost_frame = lost_frame.wrapping_add(1);
                }
                frm_pktlost[fidx] = frm_pktlost[fidx].wrapping_add(1);
                pkts_stat[idx] = pktsend_tp::snd_lost;
            }

            *last_ack = last_ack.wrapping_add(1);
        }

        num_rtt
    }

    /// Port of `rfc8888ack_t::set_stat()`.
    ///
    /// `seq` is updated to the next un-ACKed packet sequence.
    /// Returns the packet size in bytes.
    #[allow(clippy::too_many_arguments)]
    pub fn set_stat(
        &mut self,
        seq: &mut count_tp,
        maxseq: count_tp,
        now: time_tp,
        recvtime: &mut [time_tp; PKT_BUFFER_SIZE],
        recvecn: &mut [ecn_tp; PKT_BUFFER_SIZE],
        recvseq: &mut [pktrecv_tp; PKT_BUFFER_SIZE],
        maxpkt: size_tp,
    ) -> u16 {
        let mut rptsize: u16 = Self::HEADER_SIZE as u16;
        let avail = (maxpkt as u16).wrapping_sub(rptsize);
        let max_reports_by_mtu = (avail as usize) / 2;
        let remaining = maxseq.wrapping_sub(*seq);
        let reports = if remaining > max_reports_by_mtu as i32 {
            max_reports_by_mtu as u16
        } else {
            remaining as u16
        };

        let begin_seq = *seq;
        write_i32_ne(self.buf, 1, begin_seq);

        for i in 0..reports {
            let idx = idx_pkt(begin_seq.wrapping_add(i as i32));
            let ok = match recvseq[idx] {
                pktrecv_tp::rcv_recv => true,
                pktrecv_tp::rcv_ackd => {
                    recvtime[idx].wrapping_add(RCV_TIMEOUT).wrapping_sub(now) > 0
                }
                _ => false,
            };

            if ok {
                let ecn_bits = (recvecn[idx] as u16) & (ecn_tp::ecn_ce as u16);
                let ato = ((now.wrapping_sub(recvtime[idx]).wrapping_add(1 << 9)) >> 10) as u16;
                let rep = (0x1 << 15) | (ecn_bits << 13) | (ato & 0x1FFF);
                self.set_report(i, rep);
                // htons
                swap2(self.buf, Self::report_off(i));
                recvseq[idx] = pktrecv_tp::rcv_ackd;
            } else {
                self.set_report(i, 0);
                swap2(self.buf, Self::report_off(i));
                recvseq[idx] = pktrecv_tp::rcv_lost;
            }
            rptsize = rptsize.wrapping_add(2);
            *seq = seq.wrapping_add(1);
        }

        write_u8(self.buf, 0, RFC8888_ACK_TYPE);
        write_u16_ne(self.buf, 5, reports);

        // swap header to network order
        swap4(self.buf, 1);
        swap2(self.buf, 5);

        rptsize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_reference() {
        assert_eq!(DataMessage::SIZE, 13);
        assert_eq!(FrameMessage::SIZE, 25);
        assert_eq!(AckMessage::SIZE, 26);
        assert_eq!(Rfc8888Ack::HEADER_SIZE, 7);
    }

    #[test]
    fn data_message_hton_roundtrip_matches_network_order() {
        let mut buf = [0u8; DataMessage::SIZE];
        {
            let mut m = DataMessage::new(&mut buf).unwrap();
            m.set_timestamp(0x01020304);
            m.set_echoed_timestamp(0x0a0b0c0d);
            m.set_seq_nr(0x10203040);
            m.hton();
        }

        // Network order bytes are big-endian.
        assert_eq!(buf[0], BULK_DATA_TYPE);
        assert_eq!(&buf[1..5], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&buf[5..9], &[0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(&buf[9..13], &[0x10, 0x20, 0x30, 0x40]);

        // Roundtrip: swap again yields original host value.
        let mut m = DataMessage::new(&mut buf).unwrap();
        m.hton();
        assert_eq!(m.timestamp(), 0x01020304);
        assert_eq!(m.echoed_timestamp(), 0x0a0b0c0d);
        assert_eq!(m.seq_nr(), 0x10203040);
    }

    #[test]
    fn decode_data_message_network_matches_encoded_bytes() {
        let mut buf = [0u8; DataMessage::SIZE];
        encode_data_message_network(&mut buf, 0x01020304, 0x0a0b0c0d, 0x10203040).unwrap();

        let (timestamp, echoed_timestamp, seq_nr) = decode_data_message_network(&buf).unwrap();
        assert_eq!(timestamp, 0x01020304);
        assert_eq!(echoed_timestamp, 0x0a0b0c0d);
        assert_eq!(seq_nr, 0x10203040);
    }

    #[test]
    fn frame_message_hton_roundtrip_matches_network_order() {
        let mut buf = [0u8; FrameMessage::SIZE];
        {
            let mut message = FrameMessage::new(&mut buf).unwrap();
            message.set_timestamp(0x01020304);
            message.set_echoed_timestamp(0x0a0b0c0d);
            message.set_seq_nr(0x10203040);
            message.set_frame_nr(0x11223344);
            message.set_frame_sent(0x55667788);
            message.set_frame_size(0x01020305);
            message.hton();
        }

        assert_eq!(buf[0], RT_DATA_TYPE);
        assert_eq!(&buf[1..5], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&buf[5..9], &[0x0a, 0x0b, 0x0c, 0x0d]);
        assert_eq!(&buf[9..13], &[0x10, 0x20, 0x30, 0x40]);
        assert_eq!(&buf[13..17], &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(&buf[17..21], &[0x55, 0x66, 0x77, 0x88]);
        assert_eq!(&buf[21..25], &[0x01, 0x02, 0x03, 0x05]);

        let mut message = FrameMessage::new(&mut buf).unwrap();
        message.hton();
        assert_eq!(message.timestamp(), 0x01020304);
        assert_eq!(message.echoed_timestamp(), 0x0a0b0c0d);
        assert_eq!(message.seq_nr(), 0x10203040);
        assert_eq!(message.frame_nr(), 0x11223344);
        assert_eq!(message.frame_sent(), 0x55667788);
        assert_eq!(message.frame_size(), 0x01020305);
    }

    #[test]
    fn decode_frame_message_network_matches_encoded_bytes() {
        let mut buf = [0u8; FrameMessage::SIZE];
        encode_frame_message_network(
            &mut buf, 0x01020304, 0x0a0b0c0d, 0x10203040, 0x11223344, 0x55667788, 0x01020305,
        )
        .unwrap();

        let (timestamp, echoed_timestamp, seq_nr, frame_nr, frame_sent, frame_size) =
            decode_frame_message_network(&buf).unwrap();
        assert_eq!(timestamp, 0x01020304);
        assert_eq!(echoed_timestamp, 0x0a0b0c0d);
        assert_eq!(seq_nr, 0x10203040);
        assert_eq!(frame_nr, 0x11223344);
        assert_eq!(frame_sent, 0x55667788);
        assert_eq!(frame_size, 0x01020305);
    }

    #[test]
    fn ack_message_set_stat_and_get_stat_roundtrip() {
        let mut buf = [0u8; AckMessage::SIZE];
        let mut pkts_stat = [pktsend_tp::snd_init; PKT_BUFFER_SIZE];
        // Mark a window of packets as sent.
        for i in 1..=10 {
            pkts_stat[idx_pkt(i)] = pktsend_tp::snd_sent;
        }
        let mut lost_state: count_tp = 0;

        {
            let mut a = AckMessage::new(&mut buf).unwrap();
            a.set_ack_seq(10);
            a.set_timestamp(123);
            a.set_echoed_timestamp(100);
            a.set_packets_received(10);
            a.set_packets_CE(0);
            a.set_packets_lost(2);
            a.set_error_L4S(false);
            a.set_stat();
        }

        // Decode and update.
        let mut a = AckMessage::new(&mut buf).unwrap();
        a.get_stat(&mut pkts_stat, &mut lost_state);
        assert_eq!(a.ack_seq(), 10);
        assert_eq!(a.packets_lost(), 2);
        assert_eq!(lost_state, 2);
        assert_eq!(pkts_stat[idx_pkt(10)], pktsend_tp::snd_recv);
        assert_eq!(pkts_stat[idx_pkt(9)], pktsend_tp::snd_lost);
        assert_eq!(pkts_stat[idx_pkt(8)], pktsend_tp::snd_lost);
    }

    #[test]
    fn encode_ack_message_network_matches_get_stat_roundtrip() {
        let mut buf = [0u8; AckMessage::SIZE];
        encode_ack_message_network(&mut buf, 10, 123, 100, 10, 1, 2, true).unwrap();

        let mut ack = AckMessage::new(&mut buf).unwrap();
        let mut pkts_stat = [pktsend_tp::snd_init; PKT_BUFFER_SIZE];
        pkts_stat[idx_pkt(10)] = pktsend_tp::snd_sent;
        pkts_stat[idx_pkt(9)] = pktsend_tp::snd_sent;
        pkts_stat[idx_pkt(8)] = pktsend_tp::snd_sent;
        let mut lost_state = 0;
        ack.get_stat(&mut pkts_stat, &mut lost_state);

        assert_eq!(ack.ack_seq(), 10);
        assert_eq!(ack.timestamp(), 123);
        assert_eq!(ack.echoed_timestamp(), 100);
        assert_eq!(ack.packets_received(), 10);
        assert_eq!(ack.packets_CE(), 1);
        assert_eq!(ack.packets_lost(), 2);
        assert!(ack.error_L4S());
        assert_eq!(pkts_stat[idx_pkt(10)], pktsend_tp::snd_recv);
        assert_eq!(pkts_stat[idx_pkt(9)], pktsend_tp::snd_lost);
        assert_eq!(pkts_stat[idx_pkt(8)], pktsend_tp::snd_lost);
    }

    #[test]
    fn rfc8888_set_stat_emits_expected_report_bytes_and_advances_seq() {
        let mut buf = [0u8; 32];
        let mut recvtime = [0; PKT_BUFFER_SIZE];
        let mut recvecn = [ecn_tp::ecn_not_ect; PKT_BUFFER_SIZE];
        let mut recvseq = [pktrecv_tp::rcv_init; PKT_BUFFER_SIZE];
        let now = 10_000;
        let mut seq = 10;

        recvtime[idx_pkt(10)] = now;
        recvecn[idx_pkt(10)] = ecn_tp::ecn_l4s_id;
        recvseq[idx_pkt(10)] = pktrecv_tp::rcv_recv;

        recvtime[idx_pkt(11)] = now - 2_048;
        recvecn[idx_pkt(11)] = ecn_tp::ecn_ce;
        recvseq[idx_pkt(11)] = pktrecv_tp::rcv_ackd;

        let mut ack = Rfc8888Ack::new(&mut buf).unwrap();
        let size = ack.set_stat(
            &mut seq,
            13,
            now,
            &mut recvtime,
            &mut recvecn,
            &mut recvseq,
            64,
        );

        assert_eq!(size, 13);
        assert_eq!(seq, 13);
        assert_eq!(buf[0], RFC8888_ACK_TYPE);
        assert_eq!(&buf[1..5], &10i32.to_be_bytes());
        assert_eq!(&buf[5..7], &3u16.to_be_bytes());
        assert_eq!(&buf[7..9], &0xA000u16.to_be_bytes());
        assert_eq!(&buf[9..11], &0xE002u16.to_be_bytes());
        assert_eq!(&buf[11..13], &0u16.to_be_bytes());
        assert_eq!(recvseq[idx_pkt(10)], pktrecv_tp::rcv_ackd);
        assert_eq!(recvseq[idx_pkt(11)], pktrecv_tp::rcv_ackd);
        assert_eq!(recvseq[idx_pkt(12)], pktrecv_tp::rcv_lost);
    }

    #[test]
    fn rfc8888_set_stat_respects_mtu_report_budget() {
        let mut buf = [0u8; 32];
        let mut recvtime = [0; PKT_BUFFER_SIZE];
        let mut recvecn = [ecn_tp::ecn_l4s_id; PKT_BUFFER_SIZE];
        let mut recvseq = [pktrecv_tp::rcv_recv; PKT_BUFFER_SIZE];
        let mut seq = 20;

        let mut ack = Rfc8888Ack::new(&mut buf).unwrap();
        let size = ack.set_stat(
            &mut seq,
            25,
            50_000,
            &mut recvtime,
            &mut recvecn,
            &mut recvseq,
            11,
        );

        assert_eq!(size, 11);
        assert_eq!(seq, 22);
        assert_eq!(&buf[1..5], &20i32.to_be_bytes());
        assert_eq!(&buf[5..7], &2u16.to_be_bytes());
        assert_eq!(recvseq[idx_pkt(20)], pktrecv_tp::rcv_ackd);
        assert_eq!(recvseq[idx_pkt(21)], pktrecv_tp::rcv_ackd);
        assert_eq!(recvseq[idx_pkt(22)], pktrecv_tp::rcv_recv);
    }

    #[test]
    fn rfc8888_get_stat_marks_gap_before_begin_seq_as_lost() {
        let mut buf = [0u8; 16];
        write_u8(&mut buf, 0, RFC8888_ACK_TYPE);
        write_i32_ne(&mut buf, 1, 5);
        swap4(&mut buf, 1);
        write_u16_ne(&mut buf, 5, 1);
        swap2(&mut buf, 5);
        write_u16_ne(&mut buf, 7, 0xA000);
        swap2(&mut buf, 7);

        let sendtime = [0; PKT_BUFFER_SIZE];
        let mut pkts_rtt = [0; REPORT_SIZE];
        let mut pkts_stat = [pktsend_tp::snd_init; PKT_BUFFER_SIZE];
        pkts_stat[idx_pkt(4)] = pktsend_tp::snd_sent;
        pkts_stat[idx_pkt(5)] = pktsend_tp::snd_sent;

        let mut rcvd = 0;
        let mut lost = 0;
        let mut mark = 0;
        let mut error = false;
        let mut last_ack = 3;
        let mut ack = Rfc8888Ack::new(&mut buf).unwrap();
        let num_rtt = ack.get_stat(
            1_000,
            &sendtime,
            &mut pkts_rtt,
            &mut rcvd,
            &mut lost,
            &mut mark,
            &mut error,
            &mut pkts_stat,
            &mut last_ack,
        );

        assert_eq!(num_rtt, 1);
        assert_eq!(rcvd, 1);
        assert_eq!(lost, 1);
        assert_eq!(mark, 0);
        assert!(!error);
        assert_eq!(last_ack, 5);
        assert_eq!(pkts_stat[idx_pkt(4)], pktsend_tp::snd_lost);
        assert_eq!(pkts_stat[idx_pkt(5)], pktsend_tp::snd_recv);
        assert_eq!(pkts_rtt[0], 1_000);
    }

    #[test]
    fn rfc8888_get_stat_reclassifies_lost_packet_when_report_arrives() {
        let mut buf = [0u8; 16];
        write_u8(&mut buf, 0, RFC8888_ACK_TYPE);
        write_i32_ne(&mut buf, 1, 5);
        swap4(&mut buf, 1);
        write_u16_ne(&mut buf, 5, 1);
        swap2(&mut buf, 5);
        write_u16_ne(&mut buf, 7, 0x8001);
        swap2(&mut buf, 7);

        let mut sendtime = [0; PKT_BUFFER_SIZE];
        sendtime[idx_pkt(5)] = 400;
        let mut pkts_rtt = [0; REPORT_SIZE];
        let mut pkts_stat = [pktsend_tp::snd_init; PKT_BUFFER_SIZE];
        pkts_stat[idx_pkt(5)] = pktsend_tp::snd_lost;

        let mut rcvd = 0;
        let mut lost = 1;
        let mut mark = 0;
        let mut error = false;
        let mut last_ack = 4;
        let mut ack = Rfc8888Ack::new(&mut buf).unwrap();
        let num_rtt = ack.get_stat(
            2_000,
            &sendtime,
            &mut pkts_rtt,
            &mut rcvd,
            &mut lost,
            &mut mark,
            &mut error,
            &mut pkts_stat,
            &mut last_ack,
        );

        assert_eq!(num_rtt, 1);
        assert_eq!(rcvd, 1);
        assert_eq!(lost, 0);
        assert_eq!(mark, 0);
        assert!(error);
        assert_eq!(last_ack, 5);
        assert_eq!(pkts_stat[idx_pkt(5)], pktsend_tp::snd_recv);
        assert_eq!(pkts_rtt[0], 2_000 - 1_024 - 400);
    }
}
