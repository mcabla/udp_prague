use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

fn temp_json_path(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    env::temp_dir().join(format!("udp_prague_{label}_{unique}.jsonl"))
}

#[test]
fn rfc8888_negative_timing_samples_are_clamped_for_reporting() {
    let args = vec!["udp_prague_sender".to_string()];
    let mut app = AppStuff::new(true, &args).expect("app args");

    app.LogRecvRFC8888ACK(&crate::core::PragueRecvRfc8888AckEvent {
        now: 1_000,
        seqnr: 1,
        bytes_received: 26,
        begin_seq: 1,
        num_reports: 1,
        num_rtt: 1,
        rtts: &[-1],
        counters: crate::core::PragueAckCounters {
            packets_received: 8,
            packets_ce: 0,
            packets_lost: 0,
            error_l4s: false,
        },
        transport: crate::core::PraguePacketWindowMetrics {
            pacing_rate: 12_500,
            packet_window: 2,
            packet_burst: 1,
            packet_inflight: 0,
            packet_inburst: 1,
            next_send: 1_100,
        },
        frames: crate::core::PragueFrameWindowMetrics {
            frame_window: 0,
            frame_inflight: 0,
            frame_sending: false,
            sent_frame: 0,
            lost_frame: 0,
            recv_frame: 0,
        },
    });

    assert_eq!(app.acc_rtts, 0);
    assert_eq!(app.count_rtts, 1);
}

#[test]
fn startup_timeout_flag_populates_runner_config() {
    let args = vec![
        "udp_prague_sender".to_string(),
        "--startuptimeout".to_string(),
        "250000".to_string(),
    ];
    let app = AppStuff::new(true, &args).expect("app args");

    assert_eq!(app.runner_config().startup_wait_timeout_us, Some(250_000));
}

#[test]
fn json_dump_failure_is_latched() {
    let args = vec!["udp_prague_sender".to_string()];
    let mut app = AppStuff::new(true, &args).expect("app args");

    app.json_output = true;
    app.jw.reset();
    app.jw.field_str("name", "sender");
    app.jw.finalize();

    app.dump_json_report();
    assert!(app.json_output_failed);

    app.dump_json_report();
    assert!(app.json_output_failed);
}

#[test]
fn json_filename_rejects_path_separators_for_cxx_cli_parity() {
    let args = vec![
        "udp_prague_sender".to_string(),
        "-j".to_string(),
        "subdir/out.jsonl".to_string(),
    ];

    assert!(matches!(
        AppStuff::new(true, &args),
        Err(AppError::InvalidValue("json filename"))
    ));
}

#[test]
fn sender_rt_json_report_keeps_frame_fields_and_cxx_float_format() {
    let args = vec!["udp_prague_sender".to_string()];
    let mut app = AppStuff::new(true, &args).expect("app args");
    let path = temp_json_path("sender_rt");

    app.json_output = true;
    app.rt_mode = true;
    app.jw
        .init(path.to_str().expect("utf8 path"), false)
        .expect("init json writer");
    app.acc_bytes_sent = 125;
    app.acc_bytes_rcvd = 25;
    app.acc_rtts = 50;
    app.count_rtts = 1;

    app.PrintSender(1_000_000, 10, 0, 0, 250_000, 7, 3, 4, 2, 5, 1);

    assert!(app.jw.buf.contains("\"sent_rate\":\"0.001000\""));
    assert!(app.jw.buf.contains("\"rtt\":\"0.050000\""));
    assert!(app.jw.buf.contains("\"frame_inflight\":\"1\""));
    assert!(app.jw.buf.contains("\"frame_window\":\"5\""));
    assert!(app.jw.buf.contains("\"pkt_inflight\":\"4\""));

    let frame_pos = app.jw.buf.find("\"frame_inflight\"").expect("frame field");
    let packet_pos = app.jw.buf.find("\"pkt_inflight\"").expect("packet field");
    assert!(frame_pos < packet_pos);

    let _ = fs::remove_file(path);
}

#[test]
fn receiver_rfc8888_json_report_uses_ato_key() {
    let args = vec!["udp_prague_receiver".to_string()];
    let mut app = AppStuff::new(false, &args).expect("app args");
    let path = temp_json_path("receiver_rfc8888");

    app.json_output = true;
    app.rfc8888_ack = true;
    app.jw
        .init(path.to_str().expect("utf8 path"), false)
        .expect("init json writer");
    app.acc_bytes_rcvd = 250;
    app.acc_bytes_sent = 25;
    app.acc_rtts = 25_000;
    app.count_rtts = 1;
    app.prev_pkts = 8;
    app.prev_marks = 1;
    app.prev_losts = 2;

    app.PrintReceiver(1_000_000, 0, 0, 0);

    assert!(app.jw.buf.contains("\"ATO\":"), "{}", app.jw.buf);
    assert!(!app.jw.buf.contains("\"RTT\":"));
    assert!(app.jw.buf.contains("\"pkt_rcvd\":\"8\""));
    assert!(app.jw.buf.contains("\"pkt_mark\":\"1\""));
    assert!(app.jw.buf.contains("\"pkt_lost\":\"2\""));

    let _ = fs::remove_file(path);
}
