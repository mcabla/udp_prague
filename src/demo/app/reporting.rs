use std::time::{SystemTime, UNIX_EPOCH};

use crate::congestion::{count_tp, rate_tp, time_tp};
use crate::core::{
    PragueRecvAckEvent, PragueRecvDataEvent, PragueRecvRfc8888AckEvent, PragueSendAckEvent,
    PragueSendDataEvent, PragueSendFrameDataEvent, PragueSendRfc8888AckEvent,
};

use super::AppStuff;

fn wall_time_micros() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_micros().min(u128::from(u64::MAX)) as u64,
        Err(_) => 0,
    }
}

fn report_window_us(now: time_tp, rept_tm: time_tp, rept_int: u32) -> f32 {
    let elapsed = i64::from(now) - i64::from(rept_tm) + i64::from(rept_int);
    if elapsed > 0 {
        elapsed as f32
    } else {
        1.0
    }
}

impl AppStuff {
    pub(super) fn dump_json_report(&mut self) {
        if self.json_output_failed {
            return;
        }

        if let Err(err) = self.jw.dump() {
            self.json_output_failed = true;
            let output_path = if self.jw.file.is_empty() {
                "<uninitialized json output>"
            } else {
                self.jw.file.as_str()
            };
            eprintln!("warning: failed to write JSON report to {output_path}: {err}");
        }
    }

    /// Log send of a bulk data packet.
    pub fn LogSendData(&mut self, event: &PragueSendDataEvent) {
        if self.verbose {
            println!(
                "s: {}, {}, {}, {}, {}, {},,,,, {}, {}, {}, {}, {}, {}",
                event.now,
                event.timestamp,
                event.echoed_timestamp,
                event.timestamp.wrapping_sub(self.data_tm),
                event.seqnr,
                event.pkt_size,
                event.transport.pacing_rate,
                event.transport.packet_window,
                event.transport.packet_burst,
                event.transport.packet_inflight,
                event.transport.packet_inburst,
                event.transport.next_send.wrapping_sub(event.now)
            );
            self.data_tm = event.timestamp;
        }
        if !self.quiet {
            self.acc_bytes_sent = self.acc_bytes_sent.saturating_add(event.pkt_size);
        }
    }

    /// Log send of an RT frame packet.
    pub fn LogSendFrameData(&mut self, event: &PragueSendFrameDataEvent) {
        if self.verbose {
            println!(
                "s: {}, {}, {}, {}, {}, {},,,,, {}, {}, {}, {}, {}, {}, {}, {}",
                event.now,
                event.timestamp,
                event.echoed_timestamp,
                event.timestamp.wrapping_sub(self.data_tm),
                event.seqnr,
                event.pkt_size,
                event.pacing_rate,
                event.frame_window,
                event.frame_size,
                event.packet_burst,
                event.frame_inflight,
                event.frame_sent,
                event.packet_inburst,
                event.next_send.wrapping_sub(event.now)
            );
            self.data_tm = event.timestamp;
        }
        if !self.quiet {
            self.acc_bytes_sent = self.acc_bytes_sent.saturating_add(event.pkt_size);
        }
    }

    /// Log receipt of a data packet at the receiver.
    pub fn LogRecvData(&mut self, event: &PragueRecvDataEvent) {
        if self.verbose {
            println!(
                "r: {}, {}, {}, {}, {}, {}",
                event.now,
                event.timestamp,
                event.echoed_timestamp,
                event.timestamp.wrapping_sub(self.data_tm),
                event.seqnr,
                event.bytes_received
            );
            self.data_tm = event.timestamp;
        }
        if !self.quiet {
            self.acc_bytes_rcvd = self.acc_bytes_rcvd.saturating_add(event.bytes_received);
            if event.echoed_timestamp != 0 && !self.rfc8888_ack {
                self.record_timing_sample(event.now.wrapping_sub(event.echoed_timestamp));
            }
        }
    }

    /// Log send of a classic ACK.
    pub fn LogSendACK(&mut self, event: &PragueSendAckEvent) {
        if self.verbose {
            println!(
                "s: {}, {}, {}, {}, {}, {}, {}, {}, {}, {}",
                event.now,
                event.timestamp,
                event.echoed_timestamp,
                event.timestamp.wrapping_sub(self.ack_tm),
                event.seqnr,
                event.packet_size,
                event.counters.packets_received,
                event.counters.packets_ce,
                event.counters.packets_lost,
                event.counters.error_l4s as u8
            );
            self.ack_tm = event.timestamp;
        }
        if !self.quiet {
            self.acc_bytes_sent = self.acc_bytes_sent.saturating_add(event.packet_size);
            if event.now.wrapping_sub(self.rept_tm) >= 0 {
                self.PrintReceiver(
                    event.now,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                );
            }
        }
    }

    /// Log send of an RFC8888 ACK.
    pub fn LogSendRFC8888ACK(&mut self, event: &PragueSendRfc8888AckEvent<'_>) {
        if self.verbose {
            println!(
                "s: {}, {}, {}, {}, {}, {}",
                event.now,
                event.now.wrapping_sub(self.ack_tm),
                event.seqnr,
                event.packet_size,
                event.begin_seq,
                event.num_reports
            );
            self.ack_tm = event.now;
        }
        if !self.quiet {
            self.acc_bytes_sent = self.acc_bytes_sent.saturating_add(event.packet_size);
            for chunk in event
                .report
                .chunks_exact(2)
                .take(event.num_reports as usize)
            {
                let report = u16::from_be_bytes([chunk[0], chunk[1]]);
                if ((report & 0x8000) >> 15) != 0 {
                    self.acc_rtts = self
                        .acc_rtts
                        .saturating_add(((report & 0x1FFF) as u64) << 10);
                    self.prev_pkts = self.prev_pkts.wrapping_add(1);
                    self.prev_marks = self
                        .prev_marks
                        .wrapping_add((((report & 0x6000) >> 13) == 0x3) as count_tp);
                    self.count_rtts = self.count_rtts.wrapping_add(1);
                } else {
                    self.prev_losts = self.prev_losts.wrapping_add(1);
                }
            }
            if event.now.wrapping_sub(self.rept_tm) >= 0 {
                self.PrintReceiver(event.now, 0, 0, 0);
            }
        }
    }

    /// Log receive of a classic ACK at sender.
    pub fn LogRecvACK(&mut self, event: &PragueRecvAckEvent) {
        if self.verbose {
            if !self.rt_mode {
                println!(
                    "NORMAL_ACK_r: {}, {}, {}, {}, {}, {}, {}, {}, {}, {},,,,, {}, {}, {}",
                    event.now,
                    event.timestamp,
                    event.echoed_timestamp,
                    event.timestamp.wrapping_sub(self.ack_tm),
                    event.seqnr,
                    event.bytes_received,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.counters.error_l4s as u8,
                    event.transport.packet_inflight,
                    event.transport.packet_inburst,
                    event.transport.next_send.wrapping_sub(event.now)
                );
            } else {
                println!(
                    "NORMAL_ACK_r: {}, {}, {}, {}, {}, {}, {}, {}, {}, {},,,,, {}, {}, {}, {}, {}, {}",
                    event.now,
                    event.timestamp,
                    event.echoed_timestamp,
                    event.timestamp.wrapping_sub(self.ack_tm),
                    event.seqnr,
                    event.bytes_received,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.counters.error_l4s as u8,
                    event.frames.frame_inflight,
                    event.frames.frame_sending as u8,
                    event.frames.sent_frame,
                    event.frames.lost_frame,
                    event.frames.recv_frame,
                    event.transport.next_send.wrapping_sub(event.now)
                );
            }
            self.ack_tm = event.timestamp;
        }
        if !self.quiet {
            self.acc_bytes_rcvd = self.acc_bytes_rcvd.saturating_add(event.bytes_received);
            self.record_timing_sample(event.now.wrapping_sub(event.echoed_timestamp));
            if event.now.wrapping_sub(self.rept_tm) >= 0 {
                self.PrintSender(
                    event.now,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.transport.pacing_rate,
                    event.transport.packet_window,
                    event.transport.packet_burst,
                    event.transport.packet_inflight,
                    event.transport.packet_inburst,
                    event.frames.frame_window,
                    event.frames.frame_inflight,
                );
            }
        }
    }

    /// Log receive of an RFC8888 ACK at sender.
    pub fn LogRecvRFC8888ACK(&mut self, event: &PragueRecvRfc8888AckEvent<'_>) {
        if self.verbose {
            if !self.rt_mode {
                println!(
                    "RFC8888_ACK_r: {}, {}, {}, {}, {}, {}, {}, {}, {}, {},,,,, {}, {}, {}",
                    event.now,
                    event.begin_seq,
                    event.num_reports,
                    event.now.wrapping_sub(self.ack_tm),
                    event.seqnr,
                    event.bytes_received,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.counters.error_l4s as u8,
                    event.transport.packet_inflight,
                    event.transport.packet_inburst,
                    event.transport.next_send.wrapping_sub(event.now)
                );
                self.ack_tm = event.now;
            } else {
                println!(
                    "RFC8888_ACK_r: {}, {}, {}, {}, {}, {}, {}, {}, {}, {},,,,, {}, {}, {}, {}, {}, {}",
                    event.now,
                    event.begin_seq,
                    event.num_reports,
                    event.now.wrapping_sub(self.ack_tm),
                    event.seqnr,
                    event.bytes_received,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.counters.error_l4s as u8,
                    event.frames.frame_inflight,
                    event.frames.frame_sending as u8,
                    event.frames.sent_frame,
                    event.frames.lost_frame,
                    event.frames.recv_frame,
                    event.transport.next_send.wrapping_sub(event.now)
                );
            }
        }
        if !self.quiet {
            self.acc_bytes_rcvd = self.acc_bytes_rcvd.saturating_add(event.bytes_received);
            for &rtt in event.rtts.iter().take(event.num_rtt as usize) {
                self.record_timing_sample(rtt);
            }
            if event.now.wrapping_sub(self.rept_tm) >= 0 {
                self.PrintSender(
                    event.now,
                    event.counters.packets_received,
                    event.counters.packets_ce,
                    event.counters.packets_lost,
                    event.transport.pacing_rate,
                    event.transport.packet_window,
                    event.transport.packet_burst,
                    event.transport.packet_inflight,
                    event.transport.packet_inburst,
                    event.frames.frame_window,
                    event.frames.frame_inflight,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn PrintSender(
        &mut self,
        now: time_tp,
        packets_received: count_tp,
        packets_ce: count_tp,
        packets_lost: count_tp,
        pacing_rate: rate_tp,
        packet_window: count_tp,
        packet_burst: count_tp,
        packet_inflight: count_tp,
        packet_inburst: count_tp,
        frame_window: count_tp,
        frame_inflight: count_tp,
    ) {
        let interval = report_window_us(now, self.rept_tm, self.rept_int);
        let packets_delta = packets_received.wrapping_sub(self.prev_pkts);
        let marks_delta = packets_ce.wrapping_sub(self.prev_marks);
        let lost_delta = packets_lost.wrapping_sub(self.prev_losts);
        let rate_rcvd = 8.0 * self.acc_bytes_rcvd as f32 / interval;
        let rate_sent = 8.0 * self.acc_bytes_sent as f32 / interval;
        let rate_pacing = 8.0 * pacing_rate as f32 / 1_000_000.0;
        let rtt = if self.count_rtts > 0 {
            0.001 * self.acc_rtts as f32 / self.count_rtts as f32
        } else {
            0.0
        };
        let mark_prob = if packets_delta > 0 {
            100.0 * marks_delta as f32 / packets_delta as f32
        } else {
            0.0
        };
        let loss_prob = if packets_delta > 0 {
            100.0 * lost_delta as f32 / packets_delta as f32
        } else {
            0.0
        };

        if !self.json_output {
            if !self.rt_mode {
                println!(
                    "[SENDER]: {:.2} sec, Sent: {:.3} Mbps, Rcvd: {:.3} Mbps, RTT: {:.3} ms, Mark: {:.2}%({}/{}), Lost: {:.2}%({}/{}), Pacing rate: {:.3} Mbps, InFlight/W: {}/{} packets, InBurst/B: {}/{} packets",
                    now as f32 / 1_000_000.0,
                    rate_sent,
                    rate_rcvd,
                    rtt,
                    mark_prob,
                    marks_delta,
                    packets_delta,
                    loss_prob,
                    lost_delta,
                    packets_delta,
                    rate_pacing,
                    packet_inflight,
                    packet_window,
                    packet_inburst,
                    packet_burst
                );
            } else {
                println!(
                    "[RT-SENDER]: {:.2} sec, Sent: {:.3} Mbps, Rcvd: {:.3} Mbps, RTT: {:.3} ms, Mark: {:.2}%({}/{}), Lost: {:.2}%({}/{}), Pacing rate: {:.3} Mbps, FrameInFlight/W: {}/{} frames, InFlight/W: {}/{} packets, InBurst/B: {}/{} packets",
                    now as f32 / 1_000_000.0,
                    rate_sent,
                    rate_rcvd,
                    rtt,
                    mark_prob,
                    marks_delta,
                    packets_delta,
                    loss_prob,
                    lost_delta,
                    packets_delta,
                    rate_pacing,
                    frame_inflight,
                    frame_window,
                    packet_inflight,
                    packet_window,
                    packet_inburst,
                    packet_burst
                );
            }
        } else {
            self.jw.reset();
            self.jw.field_str("name", self.rept_name.as_str());
            self.jw.field_u64("time", wall_time_micros());
            self.jw.field_i32("time_since_start", now);
            self.jw.field_f32("sent_rate", rate_sent);
            self.jw.field_f32("rcvd_rate", rate_rcvd);
            self.jw.field_f32("rtt", rtt);
            self.jw.field_f32("mark_prob", mark_prob);
            self.jw.field_f32("loss_prob", loss_prob);
            self.jw.field_i32("pkt_rcvd", packets_delta);
            self.jw.field_i32("pkt_mark", marks_delta);
            self.jw.field_i32("pkt_lost", lost_delta);
            self.jw.field_f32("pacing_rate", rate_pacing);
            if self.rt_mode {
                self.jw.field_i32("frame_inflight", frame_inflight);
                self.jw.field_i32("frame_window", frame_window);
            }
            self.jw.field_i32("pkt_inflight", packet_inflight);
            self.jw.field_i32("pkt_window", packet_window);
            self.jw.field_i32("pkt_inburst", packet_inburst);
            self.jw.field_i32("pkt_burst", packet_burst);
            self.jw.finalize();
            self.dump_json_report();
        }

        self.rept_tm = now.wrapping_add(self.rept_int as time_tp);
        self.acc_bytes_sent = 0;
        self.acc_bytes_rcvd = 0;
        self.acc_rtts = 0;
        self.count_rtts = 0;
        self.prev_pkts = packets_received;
        self.prev_marks = packets_ce;
        self.prev_losts = packets_lost;
    }

    pub fn PrintReceiver(
        &mut self,
        now: time_tp,
        packets_received: count_tp,
        packets_ce: count_tp,
        packets_lost: count_tp,
    ) {
        let interval = report_window_us(now, self.rept_tm, self.rept_int);
        let rate_rcvd = 8.0 * self.acc_bytes_rcvd as f32 / interval;
        let rate_sent = 8.0 * self.acc_bytes_sent as f32 / interval;
        let rtt = if self.count_rtts > 0 {
            0.001 * self.acc_rtts as f32 / self.count_rtts as f32
        } else {
            0.0
        };
        let packet_base = if !self.rfc8888_ack {
            packets_received.wrapping_sub(self.prev_pkts)
        } else {
            self.prev_pkts
        };
        let mark_delta = if !self.rfc8888_ack {
            packets_ce.wrapping_sub(self.prev_marks)
        } else {
            self.prev_marks
        };
        let loss_delta = if !self.rfc8888_ack {
            packets_lost.wrapping_sub(self.prev_losts)
        } else {
            self.prev_losts
        };
        let mark_prob = if packet_base > 0 {
            100.0 * mark_delta as f32 / packet_base as f32
        } else {
            0.0
        };
        let loss_prob = if packet_base > 0 {
            100.0 * loss_delta as f32 / packet_base as f32
        } else {
            0.0
        };

        if !self.json_output {
            println!(
                "[RECVER]: {:.2} sec, Rcvd: {:.3} Mbps, Sent: {:.3} Mbps, {}: {:.3} ms, Mark: {:.2}%({}/{}), Lost: {:.2}%({}/{})",
                now as f32 / 1_000_000.0,
                rate_rcvd,
                rate_sent,
                if !self.rfc8888_ack { "RTT" } else { "ATO" },
                rtt,
                mark_prob,
                mark_delta,
                packet_base,
                loss_prob,
                loss_delta,
                packet_base
            );
        } else {
            self.jw.reset();
            self.jw.field_str("name", self.rept_name.as_str());
            self.jw.field_u64("time", wall_time_micros());
            self.jw.field_i32("time_since_start", now);
            self.jw.field_f32("rcvd_rate", rate_rcvd);
            self.jw.field_f32("sent_rate", rate_sent);
            self.jw
                .field_f32(if !self.rfc8888_ack { "RTT" } else { "ATO" }, rtt);
            self.jw.field_f32("mark_prob", mark_prob);
            self.jw.field_f32("loss_prob", loss_prob);
            self.jw.field_i32("pkt_rcvd", packet_base);
            self.jw.field_i32("pkt_mark", mark_delta);
            self.jw.field_i32("pkt_lost", loss_delta);
            self.jw.finalize();
            self.dump_json_report();
        }

        self.rept_tm = now.wrapping_add(self.rept_int as time_tp);
        self.acc_bytes_rcvd = 0;
        self.acc_bytes_sent = 0;
        self.acc_rtts = 0;
        self.count_rtts = 0;
        self.prev_pkts = if !self.rfc8888_ack {
            packets_received
        } else {
            0
        };
        self.prev_marks = if !self.rfc8888_ack { packets_ce } else { 0 };
        self.prev_losts = if !self.rfc8888_ack { packets_lost } else { 0 };
    }
}
