#![cfg(feature = "session")]

use std::thread;
use std::time::Duration;

use udp_prague::core::{
    PragueReceivedPacket, PragueReceivedPacketView, PragueReceiverSession,
    PragueSegmentReceiverSession, PragueSegmentSenderSession, PragueSenderSession,
    PragueSessionConfig, PragueVideoReceiverSession, PragueVideoSenderSession,
    PragueVideoSessionConfig,
};

#[test]
fn session_wrapper_bulk_roundtrip_smoke() {
    let port = 38084u16;
    let payload = b"audio-or-message-payload".to_vec();

    let receiver = thread::spawn(move || {
        let mut receiver = PragueReceiverSession::bind("0.0.0.0", port).expect("bind receiver");
        let received = receiver
            .receive_and_ack(500_000)
            .expect("receive and ack")
            .expect("packet");

        match received.packet {
            PragueReceivedPacket::Bulk(packet) => {
                assert_eq!(packet.app_data, payload);
                assert_eq!(received.ack.acked_sequence_number, packet.sequence_number);
            }
            PragueReceivedPacket::Frame(_) => panic!("unexpected frame packet"),
        }
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender =
        PragueSenderSession::connect("127.0.0.1", port, PragueSessionConfig::default())
            .expect("connect sender");

    let sent = sender
        .send_bulk(b"audio-or-message-payload")
        .expect("send bulk");
    let feedback = sender
        .receive_feedback(500_000)
        .expect("feedback result")
        .expect("ack feedback");

    assert_eq!(feedback.acked_sequence_number, sent.sequence_number);
    assert_eq!(sender.inflight_packets(), 0);

    receiver.join().expect("receiver join");
}

#[test]
fn session_wrapper_large_bulk_blocking_roundtrip_smoke() {
    let port = 38086u16;
    let payload = (0..5000u32)
        .map(|value| (value % 251) as u8)
        .collect::<Vec<_>>();
    let expected_payload = payload.clone();

    let receiver = thread::spawn(move || {
        let mut receiver = PragueReceiverSession::bind("0.0.0.0", port).expect("bind receiver");
        let mut assembled = Vec::new();

        while assembled.len() < expected_payload.len() {
            let received = receiver
                .receive_and_ack(500_000)
                .expect("receive and ack")
                .expect("bulk packet");

            match received.packet {
                PragueReceivedPacket::Bulk(packet) => {
                    assembled.extend_from_slice(&packet.app_data);
                }
                PragueReceivedPacket::Frame(_) => panic!("unexpected frame packet"),
            }
        }

        assert_eq!(assembled, expected_payload);
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender = PragueSenderSession::connect(
        "127.0.0.1",
        port,
        PragueSessionConfig {
            max_packet_size: 256,
            ..PragueSessionConfig::default()
        },
    )
    .expect("connect sender");

    let report = sender
        .send_large_bulk_blocking(&payload, 500_000)
        .expect("send large bulk");

    assert_eq!(report.app_bytes_sent, payload.len() as u64);
    assert!(report.packets_sent > 1);
    assert_eq!(sender.inflight_packets(), 0);

    receiver.join().expect("receiver join");
}

#[test]
fn session_wrapper_borrowed_bulk_roundtrip_smoke() {
    let port = 38089u16;
    let payload = b"audio-or-message-payload".to_vec();

    let receiver = thread::spawn(move || {
        let mut receiver = PragueReceiverSession::bind("0.0.0.0", port).expect("bind receiver");
        let received = receiver
            .receive_and_ack_borrowed(500_000)
            .expect("receive and ack")
            .expect("packet");

        match received.packet {
            PragueReceivedPacketView::Bulk(packet) => {
                assert_eq!(packet.app_data, payload.as_slice());
                assert_eq!(received.ack.acked_sequence_number, packet.sequence_number);
            }
            PragueReceivedPacketView::Frame(_) => panic!("unexpected frame packet"),
        }
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender =
        PragueSenderSession::connect("127.0.0.1", port, PragueSessionConfig::default())
            .expect("connect sender");

    let sent = sender
        .send_bulk(b"audio-or-message-payload")
        .expect("send bulk");
    let feedback = sender
        .receive_feedback(500_000)
        .expect("feedback result")
        .expect("ack feedback");

    assert_eq!(feedback.acked_sequence_number, sent.sequence_number);
    assert_eq!(sender.inflight_packets(), 0);

    receiver.join().expect("receiver join");
}

#[test]
fn segmented_bulk_wrapper_roundtrip_smoke() {
    let port = 38087u16;
    let content_tag = 23u16;
    let payload = (0..7000u32)
        .map(|value| ((value * 17) % 251) as u8)
        .collect::<Vec<_>>();
    let expected_payload = payload.clone();

    let receiver = thread::spawn(move || {
        let mut receiver =
            PragueSegmentReceiverSession::bind("0.0.0.0", port).expect("bind receiver");
        let received = receiver
            .receive_segment_and_ack(500_000)
            .expect("receive segment result")
            .expect("received segment");

        assert_eq!(received.content_tag, content_tag);
        assert_eq!(received.payload, expected_payload);
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender = PragueSegmentSenderSession::connect(
        "127.0.0.1",
        port,
        PragueSessionConfig {
            max_packet_size: 256,
            ..PragueSessionConfig::default()
        },
    )
    .expect("connect sender");

    let report = sender
        .send_segment_blocking(content_tag, &payload, 500_000)
        .expect("send segment");

    assert_eq!(report.content_tag, content_tag);
    assert_eq!(report.segment_size_bytes, payload.len() as u64);
    assert!(report.packets_sent > 1);
    assert_eq!(sender.inflight_packets(), 0);

    receiver.join().expect("receiver join");
}

#[test]
fn video_session_wrapper_frame_roundtrip_smoke() {
    let port = 38085u16;
    let frame = vec![0x5a; 400];
    let expected_frame = frame.clone();

    let receiver = thread::spawn(move || {
        let mut receiver = PragueReceiverSession::bind("0.0.0.0", port).expect("bind receiver");
        let mut assembled = Vec::new();
        let mut expected_frame_len = None;

        while expected_frame_len.is_none_or(|len| assembled.len() < len) {
            let received = receiver
                .receive_and_ack(500_000)
                .expect("receive and ack")
                .expect("frame packet");

            match received.packet {
                PragueReceivedPacket::Frame(packet) => {
                    expected_frame_len.get_or_insert(packet.frame_size_bytes as usize);
                    assembled.extend_from_slice(&packet.app_data);
                }
                PragueReceivedPacket::Bulk(_) => panic!("unexpected bulk packet"),
            }
        }

        assert_eq!(assembled, expected_frame);
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender = PragueVideoSenderSession::connect(
        "127.0.0.1",
        port,
        PragueVideoSessionConfig {
            max_packet_size: 256,
            ..PragueVideoSessionConfig::default()
        },
    )
    .expect("connect video sender");

    let queued = sender.queue_frame(&frame).expect("queue frame");
    assert_eq!(queued.actual_frame_size_bytes, frame.len() as u64);

    while sender.has_pending_frame() || sender.inflight_packets() > 0 {
        match sender.transmit_ready_frame_fragments() {
            Ok(Some(_report)) => {}
            Ok(None) => {}
            Err(udp_prague::SessionError::WouldBlock { .. }) => {}
            Err(err) => panic!("unexpected transmit error: {err}"),
        }

        if let Some(_feedback) = sender
            .receive_feedback(20_000)
            .expect("video feedback result")
        {}
    }

    assert_eq!(sender.inflight_packets(), 0);
    assert_eq!(sender.inflight_frames(), 0);

    receiver.join().expect("receiver join");
}

#[test]
fn video_receiver_wrapper_reassembles_full_frame_smoke() {
    let port = 38088u16;
    let frame = (0..1600u32)
        .map(|value| ((value * 19) % 251) as u8)
        .collect::<Vec<_>>();
    let expected_frame = frame.clone();

    let receiver = thread::spawn(move || {
        let mut receiver =
            PragueVideoReceiverSession::bind("0.0.0.0", port).expect("bind video receiver");
        let received = receiver
            .receive_frame_and_ack(500_000)
            .expect("receive frame result")
            .expect("received frame");

        assert_eq!(received.frame_size_bytes, expected_frame.len() as u64);
        assert_eq!(received.payload, expected_frame);
    });

    thread::sleep(Duration::from_millis(50));

    let mut sender = PragueVideoSenderSession::connect(
        "127.0.0.1",
        port,
        PragueVideoSessionConfig {
            max_packet_size: 256,
            ..PragueVideoSessionConfig::default()
        },
    )
    .expect("connect video sender");

    let queued = sender.queue_frame(&frame).expect("queue frame");
    assert_eq!(queued.actual_frame_size_bytes, frame.len() as u64);

    while sender.has_pending_frame() || sender.inflight_packets() > 0 {
        match sender.transmit_ready_frame_fragments() {
            Ok(Some(_report)) => {}
            Ok(None) => {}
            Err(udp_prague::SessionError::WouldBlock { .. }) => {}
            Err(err) => panic!("unexpected transmit error: {err}"),
        }

        if let Some(_feedback) = sender
            .receive_feedback(20_000)
            .expect("video feedback result")
        {}
    }

    assert_eq!(sender.inflight_packets(), 0);
    assert_eq!(sender.inflight_frames(), 0);

    receiver.join().expect("receiver join");
}
