// SPDX-License-Identifier: Apache-2.0
//
// The compatibility loops are adapted from L4STeam/udp_prague (Apache-2.0)
// and feed the GPL-2.0-only Classic-AQM monitor documented in NOTICE.

//! Sender/receiver main loops.
//!
//! The reference project implements its example executables as two standalone
//! `main()` functions. To enable equivalence/integration tests without
//! spawning external processes, this crate exposes the core loops as library
//! functions.
//!
//! - The binaries call these functions with `stop_after = None`.
//! - Tests may pass a bounded `stop_after` to terminate deterministically.

use super::runtime::{
    PragueAckCounters, PragueFrameWindowMetrics, PraguePacketWindowMetrics, PragueRecvAckEvent,
    PragueRecvDataEvent, PragueRecvRfc8888AckEvent, PragueSendAckEvent, PragueSendDataEvent,
    PragueSendFrameDataEvent, PragueSendRfc8888AckEvent,
};
use super::{Reporter, RunnerConfig, RunnerError};
use crate::congestion::{
    count_tp, ecn_tp, fps_tp, rate_tp, size_tp, time_tp, ClassicAqmObservation, PragueCC,
    PragueState, PRAGUE_INITRATE, PRAGUE_INITWIN, PRAGUE_MINMTU, PRAGUE_MINRATE,
};
#[cfg(feature = "demo-app")]
use crate::demo::AppStuff;
use crate::net::UDPSocket;
use crate::protocol::pkt_format::*;
use std::time::{Duration, Instant};

/// Feed one accepted sender feedback extent into the passive Classic-AQM
/// detector and publish its decision to the reporter. The detector is kept
/// out of the wire format: it consumes only validated ACK/RTT observations.
#[allow(clippy::too_many_arguments)]
fn observe_classic_aqm_feedback(
    cc: &mut PragueCC,
    reporter: &mut dyn Reporter,
    now: time_tp,
    previous: PragueState,
    packets_received: count_tp,
    packets_ce: count_tp,
    packets_lost: count_tp,
    app_limited: bool,
    min_rtt_us: &mut time_tp,
) {
    let state = *cc.GetStatePtr();
    if state.m_rtt <= 0 {
        return;
    }

    let acked_delta = packets_received.wrapping_sub(previous.m_packets_received);
    let ce_delta = packets_ce.wrapping_sub(previous.m_packets_CE);
    let lost_delta = packets_lost.wrapping_sub(previous.m_packets_lost);
    // A stale/reordered feedback block must not become a huge unsigned
    // detector extent. Prague itself rejects these counters in ACKReceived.
    if acked_delta < 0 || ce_delta < 0 || lost_delta < 0 {
        return;
    }

    if *min_rtt_us <= 0 || state.m_rtt < *min_rtt_us {
        *min_rtt_us = state.m_rtt;
    }

    let assessment = cc.observe_classic_aqm(ClassicAqmObservation {
        latest_rtt: Duration::from_micros(state.m_rtt as u64),
        min_rtt: Duration::from_micros((*min_rtt_us).max(1) as u64),
        ce_seen: ce_delta > 0,
        ce_delta: ce_delta as u64,
        acked_delta: acked_delta as u64,
        app_limited,
        congestion_avoidance_stable: previous.m_cc_state == crate::congestion::cs_tp::cs_cong_avoid,
        ..ClassicAqmObservation::default()
    });

    reporter.LogClassicAqm(&super::runtime::PragueClassicAqmEvent {
        now,
        latest_rtt_us: state.m_rtt,
        min_rtt_us: *min_rtt_us,
        acked_delta,
        ce_delta,
        app_limited,
        congestion_avoidance_stable: previous.m_cc_state == crate::congestion::cs_tp::cs_cong_avoid,
        assessment,
    });
}

/// Maximum number of consecutive timeouts before aborting.
///
/// Matches `#define MAX_TIMEOUT 2` in the reference sender.
pub const MAX_TIMEOUT: u8 = 2;

#[inline]
fn bool_as_count(value: bool) -> count_tp {
    if value {
        1
    } else {
        0
    }
}

fn wait_for_startup_trigger(
    socket: &mut UDPSocket,
    receivebuffer: &mut [u8],
    rcv_ecn: &mut ecn_tp,
    startup_wait_timeout_us: Option<u32>,
) -> Result<(), RunnerError> {
    match startup_wait_timeout_us {
        None => loop {
            let bytes_received = socket.Receive(receivebuffer, rcv_ecn, 0)?;
            if bytes_received != 0 {
                return Ok(());
            }
        },
        Some(waited_us) => {
            let total_wait = Duration::from_micros(u64::from(waited_us));
            let start = Instant::now();

            loop {
                let elapsed = start.elapsed();
                if elapsed >= total_wait {
                    return Err(RunnerError::StartupTriggerTimeout { waited_us });
                }

                let remaining = total_wait.saturating_sub(elapsed);
                let remaining_us = remaining.as_micros().min(i32::MAX as u128) as time_tp;
                let timeout_us = if remaining_us > 0 { remaining_us } else { 1 };
                let bytes_received = socket.Receive(receivebuffer, rcv_ecn, timeout_us)?;
                if bytes_received != 0 {
                    return Ok(());
                }
            }
        }
    }
}

/// Run the receiver loop.
///
/// If `stop_after_packets` is `Some(n)`, the loop exits after receiving `n`
/// data packets (used for tests).
#[cfg(feature = "demo-app")]
pub fn run_receiver(mut app: AppStuff, stop_after_packets: Option<u64>) -> Result<(), RunnerError> {
    let config = app.runner_config();
    run_receiver_with_reporter(config, &mut app, stop_after_packets)
}

/// Run the receiver loop using an explicit runtime config and reporter.
pub fn run_receiver_with_reporter(
    mut config: RunnerConfig,
    reporter: &mut dyn Reporter,
    stop_after_packets: Option<u64>,
) -> Result<(), RunnerError> {
    let mut us = UDPSocket::new();
    if config.connect {
        us.Connect(&config.rcv_addr, config.rcv_port as u16)?;
    } else {
        us.Bind(&config.rcv_addr, config.rcv_port as u16)?;
    }

    let mut receivebuffer = [0u8; BUFFER_SIZE];
    let mut ackbuf = [0u8; AckMessage::SIZE];

    let mut pragueCC = PragueCC::default();
    let mut now: time_tp = pragueCC.Now();
    let mut new_ecn: ecn_tp = ecn_tp::ecn_not_ect;

    // RFC8888 state.
    let mut rfc8888_buf = [0u8; MAX_MTU];
    let mut start_seq: count_tp = 0;
    let mut end_seq: count_tp = 0;
    let mut rfc8888_acktime: time_tp = now.wrapping_add(config.rfc8888_ackperiod as time_tp);
    let mut recvtime: [time_tp; PKT_BUFFER_SIZE] = [0; PKT_BUFFER_SIZE];
    let mut recvecn = [ecn_tp::ecn_not_ect; PKT_BUFFER_SIZE];
    let mut recvseq = [pktrecv_tp::rcv_init; PKT_BUFFER_SIZE];

    if config.rfc8888_ack {
        let rfc_view = Rfc8888Ack::new(&mut rfc8888_buf[..])?;
        let min_ack = rfc_view.get_size(1) as size_tp;
        if config.max_pkt < min_ack {
            config.max_pkt = min_ack;
        }
    }

    // Trigger ACK in connect mode.
    if config.connect {
        let (mut ts, mut ets) = (0, 0);
        pragueCC.GetTimeInfo(&mut ts, &mut ets, &mut new_ecn);
        let (mut pr, mut pc, mut pl, mut err) = (0, 0, 0, false);
        pragueCC.GetACKInfo(&mut pr, &mut pc, &mut pl, &mut err);
        encode_ack_message_network(&mut ackbuf, 0, ts, ets, pr, pc, pl, err)?;
        us.Send(&ackbuf, AckMessage::SIZE as size_tp, new_ecn)?;
    }

    let mut rx_count: u64 = 0;
    let mut pending_stop = false;
    let mut last_seq: count_tp = 0;
    loop {
        let mut should_stop = pending_stop;
        now = pragueCC.Now();

        let mut rcv_ecn = ecn_tp::ecn_not_ect;

        let wait_time: time_tp = if config.rfc8888_ack && start_seq != end_seq {
            let d = rfc8888_acktime.wrapping_sub(now);
            if d > 0 {
                d
            } else {
                1
            }
        } else {
            0
        };

        // Repeat if timeout or interrupted, matching C++ do/while.
        let bytes_received: size_tp = loop {
            let bytes_received = us.Receive(&mut receivebuffer[..], &mut rcv_ecn, wait_time)?;
            if !(bytes_received == 0 && wait_time == 0) {
                break bytes_received;
            }
        };

        if bytes_received != 0 {
            now = pragueCC.Now();
            let (ts, ets, seq) =
                decode_data_message_network(&receivebuffer[..bytes_received as usize])?;
            last_seq = seq;
            reporter.LogRecvData(&PragueRecvDataEvent {
                now,
                timestamp: ts,
                echoed_timestamp: ets,
                seqnr: seq,
                bytes_received,
            });

            if config.rfc8888_ack {
                let seq_idx = (seq as u32 as usize) % PKT_BUFFER_SIZE;
                if start_seq == end_seq {
                    start_seq = seq;
                    end_seq = seq.wrapping_add(1);
                } else {
                    // Same wrap-safe comparisons as C++.
                    if start_seq.wrapping_sub(seq) <= 0
                        && start_seq
                            .wrapping_add(PKT_BUFFER_SIZE as i32)
                            .wrapping_sub(seq)
                            > 0
                        && seq.wrapping_add(1).wrapping_sub(end_seq) > 0
                    {
                        end_seq = seq.wrapping_add(1);
                    } else if end_seq.wrapping_sub(seq) > 0
                        && end_seq
                            .wrapping_sub(PKT_BUFFER_SIZE as i32)
                            .wrapping_sub(seq)
                            <= 0
                        && seq.wrapping_sub(start_seq) < 0
                    {
                        start_seq = seq;
                    }
                }

                if recvseq[seq_idx] != pktrecv_tp::rcv_recv {
                    recvtime[seq_idx] = now;
                    // Store only CE bit (ecn_ce or not_ect), matching `ecn_tp(rcv_ecn & ecn_ce)`.
                    recvecn[seq_idx] = if rcv_ecn == ecn_tp::ecn_ce {
                        ecn_tp::ecn_ce
                    } else {
                        ecn_tp::ecn_not_ect
                    };
                    recvseq[seq_idx] = pktrecv_tp::rcv_recv;
                } else {
                    // Once CE, stay CE.
                    if rcv_ecn == ecn_tp::ecn_ce {
                        recvecn[seq_idx] = ecn_tp::ecn_ce;
                    }
                }
            }

            pragueCC.PacketReceived(ts, ets);
            pragueCC.DataReceivedSequence(rcv_ecn, seq);

            rx_count += 1;
            if let Some(limit) = stop_after_packets {
                if rx_count >= limit {
                    should_stop = true;
                    pending_stop = true;
                }
            }
        }

        now = pragueCC.Now();

        if !config.rfc8888_ack {
            // Send ACK for last received packet (mirrors C++ even if timeout case).
            let (mut ts, mut ets) = (0, 0);
            pragueCC.GetTimeInfo(&mut ts, &mut ets, &mut new_ecn);
            let (mut pr, mut pc, mut pl, mut err) = (0, 0, 0, false);
            pragueCC.GetACKInfo(&mut pr, &mut pc, &mut pl, &mut err);
            reporter.LogSendACK(&PragueSendAckEvent {
                now,
                timestamp: ts,
                echoed_timestamp: ets,
                seqnr: last_seq,
                packet_size: AckMessage::SIZE as size_tp,
                counters: PragueAckCounters {
                    packets_received: pr,
                    packets_ce: pc,
                    packets_lost: pl,
                    error_l4s: err,
                },
            });
            encode_ack_message_network(&mut ackbuf, last_seq, ts, ets, pr, pc, pl, err)?;
            us.Send(&ackbuf, AckMessage::SIZE as size_tp, new_ecn)?;
            if should_stop {
                return Ok(());
            }
        } else if rfc8888_acktime.wrapping_sub(now) <= 0 {
            while start_seq != end_seq {
                let mut rfc = Rfc8888Ack::new(&mut rfc8888_buf[..])?;
                let begin_seq = start_seq;
                let ack_size = rfc.set_stat(
                    &mut start_seq,
                    end_seq,
                    now,
                    &mut recvtime,
                    &mut recvecn,
                    &mut recvseq,
                    config.max_pkt,
                );
                us.Send(&rfc8888_buf[..], ack_size as size_tp, ecn_tp::ecn_l4s_id)?;
                let num_reports = u16::from_be_bytes(rfc8888_buf[5..7].try_into().unwrap());
                reporter.LogSendRFC8888ACK(&PragueSendRfc8888AckEvent {
                    now,
                    seqnr: 0,
                    packet_size: ack_size as size_tp,
                    begin_seq,
                    num_reports,
                    report: &rfc8888_buf[Rfc8888Ack::HEADER_SIZE..ack_size as usize],
                });
            }
            rfc8888_acktime = now.wrapping_add(config.rfc8888_ackperiod as time_tp);
            if should_stop {
                return Ok(());
            }
        } else if should_stop {
            rfc8888_acktime = now;
            pending_stop = true;
        }
    }
}

/// Run the sender loop.
///
/// If `stop_after_acks` is `Some(n)`, the loop exits after processing `n`
/// ACK events (used for tests).
#[cfg(feature = "demo-app")]
pub fn run_sender(mut app: AppStuff, stop_after_acks: Option<u64>) -> Result<(), RunnerError> {
    let config = app.runner_config();
    run_sender_with_reporter(config, &mut app, stop_after_acks)
}

/// Run the sender loop using an explicit runtime config and reporter.
pub fn run_sender_with_reporter(
    config: RunnerConfig,
    reporter: &mut dyn Reporter,
    stop_after_acks: Option<u64>,
) -> Result<(), RunnerError> {
    let mut us = UDPSocket::new();
    if config.connect {
        us.Connect(&config.rcv_addr, config.rcv_port as u16)?;
    } else {
        us.Bind(&config.rcv_addr, config.rcv_port as u16)?;
    }

    let mut receivebuffer = [0u8; BUFFER_SIZE];
    let mut sendbuffer = [0u8; BUFFER_SIZE];
    // Dummy payload.
    for (i, chunk) in sendbuffer.chunks_exact_mut(4).enumerate() {
        chunk.copy_from_slice(&(i as u32).to_be_bytes());
    }

    let mut sendtime: [time_tp; PKT_BUFFER_SIZE] = [0; PKT_BUFFER_SIZE];
    let mut pkts_stat = [pktsend_tp::snd_init; PKT_BUFFER_SIZE];
    let mut pkts_rtt: [time_tp; REPORT_SIZE] = [0; REPORT_SIZE];
    let mut last_ackseq: count_tp = 0;
    let mut pkts_received: count_tp = 0;
    let mut pkts_CE: count_tp = 0;
    let mut pkts_lost: count_tp = 0;
    let mut err_L4S: bool = false;

    let mut pragueCC = PragueCC::new(
        config.max_pkt,
        if config.rt_mode {
            config.rt_fps
        } else {
            0 as fps_tp
        },
        if config.rt_mode {
            config.rt_frameduration as time_tp
        } else {
            0
        },
        PRAGUE_INITRATE,
        PRAGUE_INITWIN,
        PRAGUE_MINRATE,
        config.max_rate,
    );
    pragueCC.set_classic_aqm_fallback_enabled(config.classic_aqm_fallback_enabled);

    let mut now = pragueCC.Now();
    let mut nextSend = now;
    let mut compRecv: time_tp = 0;
    let mut seqnr: count_tp = 0;
    let mut inflight: count_tp = 0;
    let mut pacing_rate: rate_tp = 0;
    let mut packet_window: count_tp = 0;
    let mut packet_burst: count_tp = 0;
    let mut packet_size: size_tp = 0;
    let mut new_ecn: ecn_tp = ecn_tp::ecn_not_ect;
    let mut rcv_ecn: ecn_tp = ecn_tp::ecn_not_ect;
    let mut bytes_received: size_tp;
    let mut waitTimeout: time_tp;

    // RT mode state.
    let mut frame_nr: count_tp = 0;
    let mut frame_sent: size_tp = 0;
    let mut frame_size: size_tp = 0;
    let mut frame_window: count_tp = 0;
    let mut frame_inflight: count_tp = 0;
    let mut is_sending = false;
    let mut sent_frame: count_tp = 0;
    let mut recv_frame: count_tp = 0;
    let mut lost_frame: count_tp = 0;
    let mut frame_timer: time_tp = 0;
    let mut frame_idx: [count_tp; PKT_BUFFER_SIZE] = [0; PKT_BUFFER_SIZE];
    let mut frame_pktlost: [count_tp; FRM_BUFFER_SIZE] = [0; FRM_BUFFER_SIZE];
    let mut frame_pktsent: [count_tp; FRM_BUFFER_SIZE] = [0; FRM_BUFFER_SIZE];

    let mut num_timeout: u8 = 0;
    let mut min_rtt_us: time_tp = 0;

    // Wait for a trigger packet when not connected.
    //
    // `None` preserves reference parity and blocks indefinitely. Library and CLI
    // users may opt into a bounded wait via `startup_wait_timeout_us`.
    if !config.connect {
        wait_for_startup_trigger(
            &mut us,
            &mut receivebuffer[..],
            &mut rcv_ecn,
            config.startup_wait_timeout_us,
        )?;
    }

    pragueCC.GetCCInfo(
        &mut pacing_rate,
        &mut packet_window,
        &mut packet_burst,
        &mut packet_size,
    );

    let mut ack_events: u64 = 0;
    loop {
        let mut inburst: count_tp = 0;
        let mut startSend: time_tp = 0;
        now = pragueCC.Now();

        if !config.rt_mode {
            while inflight < packet_window
                && inburst < packet_burst
                && nextSend.wrapping_sub(now) <= 0
            {
                let (mut ts, mut ets) = (0, 0);
                pragueCC.GetTimeInfo(&mut ts, &mut ets, &mut new_ecn);
                if startSend == 0 {
                    startSend = now;
                }
                seqnr = seqnr.wrapping_add(1);
                encode_data_message_network(&mut sendbuffer[..], ts, ets, seqnr)?;
                reporter.LogSendData(&PragueSendDataEvent {
                    now,
                    timestamp: ts,
                    echoed_timestamp: ets,
                    seqnr,
                    pkt_size: packet_size,
                    transport: PraguePacketWindowMetrics {
                        pacing_rate,
                        packet_window,
                        packet_burst,
                        packet_inflight: inflight,
                        packet_inburst: inburst,
                        next_send: nextSend,
                    },
                });
                us.Send(&sendbuffer, packet_size, new_ecn)?;
                sendtime[(seqnr as u32 as usize) % PKT_BUFFER_SIZE] = startSend;
                pkts_stat[(seqnr as u32 as usize) % PKT_BUFFER_SIZE] = pktsend_tp::snd_sent;
                inburst = inburst.wrapping_add(1);
                inflight = inflight.wrapping_add(1);
            }
            if startSend != 0 {
                let delta = compRecv.wrapping_add(
                    ((packet_size as i64) * (inburst as i64) * 1_000_000
                        / pacing_rate.max(1) as i64) as i32,
                );
                if delta <= 0 {
                    nextSend = startSend.wrapping_add(1);
                } else {
                    nextSend = startSend.wrapping_add(delta);
                }
                compRecv = 0;
            }
        } else {
            if frame_sent == 0 && nextSend.wrapping_sub(now) <= 0 {
                let frame_interval = (1_000_000u32 / u32::from(config.rt_fps.max(1))) as time_tp;
                if frame_timer == 0 {
                    frame_nr = frame_nr.wrapping_add(1);
                    frame_timer = now.wrapping_add(frame_interval);
                } else {
                    let mut frame_adv: count_tp = 1;
                    if frame_timer.wrapping_sub(now) <= 0 {
                        frame_adv = 1
                            + ((now.wrapping_sub(frame_timer) as i64) * (config.rt_fps as i64)
                                / 1_000_000) as i32;
                    }
                    frame_nr = frame_nr.wrapping_add(frame_adv);
                    frame_timer = frame_timer
                        .wrapping_add(((frame_adv as i64) * (frame_interval as i64)) as i32);
                }
                compRecv = 0;
                pragueCC.GetCCInfoVideo(
                    &mut pacing_rate,
                    &mut frame_size,
                    &mut frame_window,
                    &mut packet_burst,
                    &mut packet_size,
                );
            }

            while frame_inflight <= frame_window
                && frame_sent < frame_size
                && inburst < packet_burst
                && nextSend.wrapping_sub(now) <= 0
            {
                let (mut ts, mut ets) = (0, 0);
                pragueCC.GetTimeInfo(&mut ts, &mut ets, &mut new_ecn);
                if frame_sent == 0 {
                    is_sending = true;
                    frame_pktlost[(frame_nr as u32 as usize) % FRM_BUFFER_SIZE] = 0;
                    frame_pktsent[(frame_nr as u32 as usize) % FRM_BUFFER_SIZE] = 0;
                }
                if startSend == 0 {
                    startSend = now;
                }
                seqnr = seqnr.wrapping_add(1);
                let mut send_len = packet_size;
                if frame_sent.wrapping_add(packet_size) > frame_size {
                    send_len = if frame_sent.wrapping_add(PRAGUE_MINMTU) > frame_size {
                        PRAGUE_MINMTU
                    } else {
                        frame_size.wrapping_sub(frame_sent)
                    };
                }
                encode_frame_message_network(
                    &mut sendbuffer[..],
                    ts,
                    ets,
                    seqnr,
                    frame_nr,
                    frame_sent as count_tp,
                    frame_size as count_tp,
                )?;
                reporter.LogSendFrameData(&PragueSendFrameDataEvent {
                    now,
                    timestamp: ts,
                    echoed_timestamp: ets,
                    seqnr,
                    pkt_size: send_len,
                    pacing_rate,
                    frame_window,
                    frame_size: frame_window,
                    packet_burst,
                    frame_inflight,
                    frame_sent,
                    packet_inburst: inburst,
                    next_send: nextSend,
                });
                us.Send(&sendbuffer, send_len, new_ecn)?;
                sendtime[(seqnr as u32 as usize) % PKT_BUFFER_SIZE] = startSend;
                pkts_stat[(seqnr as u32 as usize) % PKT_BUFFER_SIZE] = pktsend_tp::snd_sent;
                frame_idx[(seqnr as u32 as usize) % PKT_BUFFER_SIZE] = frame_nr;
                inburst = inburst.wrapping_add(1);
                inflight = inflight.wrapping_add(1);
                frame_sent = frame_sent.wrapping_add(send_len);
            }

            if startSend != 0 {
                frame_pktsent[(frame_nr as u32 as usize) % FRM_BUFFER_SIZE] += inburst;
                if frame_sent >= frame_size {
                    nextSend = frame_timer;
                    frame_sent = 0;
                    is_sending = false;
                    sent_frame += 1;
                    if frame_pktlost[(frame_nr as u32 as usize) % FRM_BUFFER_SIZE] != 0 {
                        lost_frame += 1;
                    }
                } else {
                    let delta = compRecv.wrapping_add(
                        ((packet_size as i64) * (inburst as i64) * 1_000_000
                            / pacing_rate.max(1) as i64) as i32,
                    );
                    if delta <= 0 {
                        nextSend = startSend.wrapping_add(1);
                    } else {
                        nextSend = startSend.wrapping_add(delta);
                    }
                    compRecv = 0;
                }
                frame_inflight = bool_as_count(is_sending) + sent_frame - recv_frame - lost_frame;
            }
        }

        waitTimeout = nextSend;
        now = pragueCC.Now();
        if (!config.rt_mode && inflight >= packet_window)
            || (config.rt_mode && frame_inflight >= frame_window)
        {
            waitTimeout = now.wrapping_add(SND_TIMEOUT);
        }

        loop {
            let wt = {
                let d = waitTimeout.wrapping_sub(now);
                if d > 0 {
                    d
                } else {
                    1
                }
            };
            bytes_received = us.Receive(&mut receivebuffer[..], &mut rcv_ecn, wt)?;
            now = pragueCC.Now();
            if !(bytes_received == 0 && waitTimeout.wrapping_sub(now) > 0) {
                break;
            }
        }

        if bytes_received != 0
            && receivebuffer[0] == PKT_ACK_TYPE
            && (bytes_received as usize) >= AckMessage::SIZE
        {
            let mut ack = AckMessage::new(&mut receivebuffer[..])?;
            let previous_state = *pragueCC.GetStatePtr();
            if !config.rt_mode {
                ack.get_stat(&mut pkts_stat, &mut pkts_lost);
            } else {
                ack.get_frame_stat(
                    &mut pkts_stat,
                    &mut pkts_lost,
                    is_sending,
                    frame_nr,
                    &mut recv_frame,
                    &mut lost_frame,
                    &frame_idx,
                    &mut frame_pktsent,
                    &mut frame_pktlost,
                );
                frame_inflight = bool_as_count(is_sending) + sent_frame - recv_frame - lost_frame;
            }
            pragueCC.PacketReceived(ack.timestamp(), ack.echoed_timestamp());
            observe_classic_aqm_feedback(
                &mut pragueCC,
                reporter,
                now,
                previous_state,
                ack.packets_received(),
                ack.packets_CE(),
                ack.packets_lost(),
                config.rt_mode && !is_sending,
                &mut min_rtt_us,
            );
            pragueCC.ACKReceived(
                ack.packets_received(),
                ack.packets_CE(),
                ack.packets_lost(),
                seqnr,
                ack.error_L4S(),
                &mut inflight,
            );
            if !config.rt_mode {
                pragueCC.GetCCInfo(
                    &mut pacing_rate,
                    &mut packet_window,
                    &mut packet_burst,
                    &mut packet_size,
                );
            }
            reporter.LogRecvACK(&PragueRecvAckEvent {
                now,
                timestamp: ack.timestamp(),
                echoed_timestamp: ack.echoed_timestamp(),
                seqnr,
                bytes_received,
                counters: PragueAckCounters {
                    packets_received: ack.packets_received(),
                    packets_ce: ack.packets_CE(),
                    packets_lost: ack.packets_lost(),
                    error_l4s: ack.error_L4S(),
                },
                transport: PraguePacketWindowMetrics {
                    pacing_rate,
                    packet_window,
                    packet_burst,
                    packet_inflight: inflight,
                    packet_inburst: inburst,
                    next_send: nextSend,
                },
                frames: PragueFrameWindowMetrics {
                    frame_window,
                    frame_inflight,
                    frame_sending: is_sending,
                    sent_frame,
                    lost_frame,
                    recv_frame,
                },
            });
            ack_events += 1;
        } else if bytes_received != 0 && receivebuffer[0] == RFC8888_ACK_TYPE {
            // Parse RFC8888 ACK from receivebuffer.
            let mut rfc = Rfc8888Ack::new(&mut receivebuffer[..(bytes_received as usize)])?;
            let previous_state = *pragueCC.GetStatePtr();
            let num_rtt = if !config.rt_mode {
                rfc.get_stat(
                    now,
                    &sendtime,
                    &mut pkts_rtt,
                    &mut pkts_received,
                    &mut pkts_lost,
                    &mut pkts_CE,
                    &mut err_L4S,
                    &mut pkts_stat,
                    &mut last_ackseq,
                )
            } else {
                rfc.get_frame_stat(
                    now,
                    &sendtime,
                    &mut pkts_rtt,
                    &mut pkts_received,
                    &mut pkts_lost,
                    &mut pkts_CE,
                    &mut err_L4S,
                    &mut pkts_stat,
                    &mut last_ackseq,
                    is_sending,
                    frame_nr,
                    &mut recv_frame,
                    &mut lost_frame,
                    &frame_idx,
                    &mut frame_pktsent,
                    &mut frame_pktlost,
                )
            };

            if num_rtt != 0 {
                pragueCC.RFC8888Received(num_rtt as usize, &pkts_rtt);
                observe_classic_aqm_feedback(
                    &mut pragueCC,
                    reporter,
                    now,
                    previous_state,
                    pkts_received,
                    pkts_CE,
                    pkts_lost,
                    config.rt_mode && !is_sending,
                    &mut min_rtt_us,
                );
                pragueCC.ACKReceived(
                    pkts_received,
                    pkts_CE,
                    pkts_lost,
                    seqnr,
                    err_L4S,
                    &mut inflight,
                );
                if !config.rt_mode {
                    pragueCC.GetCCInfo(
                        &mut pacing_rate,
                        &mut packet_window,
                        &mut packet_burst,
                        &mut packet_size,
                    );
                }
            }
            reporter.LogRecvRFC8888ACK(&PragueRecvRfc8888AckEvent {
                now,
                seqnr,
                bytes_received,
                begin_seq: rfc.begin_seq(),
                num_reports: rfc.num_reports(),
                num_rtt,
                rtts: &pkts_rtt,
                counters: PragueAckCounters {
                    packets_received: pkts_received,
                    packets_ce: pkts_CE,
                    packets_lost: pkts_lost,
                    error_l4s: err_L4S,
                },
                transport: PraguePacketWindowMetrics {
                    pacing_rate,
                    packet_window,
                    packet_burst,
                    packet_inflight: inflight,
                    packet_inburst: inburst,
                    next_send: nextSend,
                },
                frames: PragueFrameWindowMetrics {
                    frame_window,
                    frame_inflight,
                    frame_sending: is_sending,
                    sent_frame,
                    lost_frame,
                    recv_frame,
                },
            });
            ack_events += 1;
        } else {
            // Timeout handling.
            if (!config.rt_mode && inflight >= packet_window)
                || (config.rt_mode && frame_inflight >= frame_window)
            {
                if num_timeout > MAX_TIMEOUT {
                    return Err(RunnerError::ConsecutiveTimeouts);
                }
                pragueCC.ResetCCInfo();
                if !config.rt_mode {
                    inflight = 0;
                    pragueCC.GetCCInfo(
                        &mut pacing_rate,
                        &mut packet_window,
                        &mut packet_burst,
                        &mut packet_size,
                    );
                    nextSend = now;
                } else {
                    frame_inflight = 0;
                    nextSend = now;
                    frame_sent = 0;
                    frame_timer = 0;
                }
                num_timeout = num_timeout.wrapping_add(1);
            }
        }

        // Exceed time will be compensated (except reset)
        now = pragueCC.Now();
        if waitTimeout.wrapping_sub(now) <= 0
            && ((!config.rt_mode && inflight > 0) || (config.rt_mode && frame_inflight > 0))
        {
            compRecv = compRecv.wrapping_add(waitTimeout.wrapping_sub(now));
        }

        if let Some(limit) = stop_after_acks {
            if ack_events >= limit {
                return Ok(());
            }
        }
    }
}
