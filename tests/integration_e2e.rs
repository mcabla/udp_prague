#![cfg(feature = "demo-app")]

use std::thread;
use std::time::Duration;

use udp_prague::core::{
    run_receiver, run_receiver_with_reporter, run_sender, run_sender_with_reporter, Reporter,
};
use udp_prague::demo::AppStuff;

struct SilentReporter;

impl Reporter for SilentReporter {}

/// Basic end-to-end sanity test.
///
/// This is not a performance test; it validates that the sender/receiver loops
/// can exchange packets/ACKs on localhost and terminate deterministically.
#[test]
fn sender_receiver_localhost_smoke() {
    // Use a high, non-privileged port that is unlikely to be busy in CI.
    let port = 38080u16;

    let rcv_args = vec![
        "udp_prague_receiver".to_string(),
        "-a".to_string(),
        "0.0.0.0".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-q".to_string(),
    ];
    let snd_args = vec![
        "udp_prague_sender".to_string(),
        "-a".to_string(),
        "127.0.0.1".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-c".to_string(),
        "-q".to_string(),
    ];

    let rcv_app = AppStuff::new(false, &rcv_args).expect("receiver args");
    let snd_app = AppStuff::new(true, &snd_args).expect("sender args");

    let rcv_thr = thread::spawn(move || run_receiver(rcv_app, Some(20)).expect("receiver run"));

    // Give receiver time to bind.
    thread::sleep(Duration::from_millis(50));

    let snd_thr = thread::spawn(move || run_sender(snd_app, Some(20)).expect("sender run"));

    snd_thr.join().expect("sender join");
    rcv_thr.join().expect("receiver join");
}

#[test]
fn sender_receiver_rfc8888_localhost_smoke() {
    let port = 38081u16;

    let rcv_args = vec![
        "udp_prague_receiver".to_string(),
        "--rfc8888".to_string(),
        "--rfc8888ackperiod".to_string(),
        "1000000".to_string(),
        "-a".to_string(),
        "0.0.0.0".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-q".to_string(),
    ];
    let snd_args = vec![
        "udp_prague_sender".to_string(),
        "--rfc8888".to_string(),
        "-a".to_string(),
        "127.0.0.1".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-c".to_string(),
        "-q".to_string(),
    ];

    let rcv_app = AppStuff::new(false, &rcv_args).expect("receiver args");
    let snd_app = AppStuff::new(true, &snd_args).expect("sender args");

    let rcv_thr = thread::spawn(move || run_receiver(rcv_app, Some(8)).expect("receiver run"));

    thread::sleep(Duration::from_millis(50));

    let snd_thr = thread::spawn(move || run_sender(snd_app, Some(1)).expect("sender run"));

    snd_thr.join().expect("sender join");
    rcv_thr.join().expect("receiver join");
}

#[test]
fn sender_receiver_custom_reporter_localhost_smoke() {
    let port = 38082u16;

    let rcv_args = vec![
        "udp_prague_receiver".to_string(),
        "-a".to_string(),
        "0.0.0.0".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-q".to_string(),
    ];
    let snd_args = vec![
        "udp_prague_sender".to_string(),
        "-a".to_string(),
        "127.0.0.1".to_string(),
        "-p".to_string(),
        port.to_string(),
        "-c".to_string(),
        "-q".to_string(),
    ];

    let rcv_config = AppStuff::new(false, &rcv_args)
        .expect("receiver args")
        .runner_config();
    let snd_config = AppStuff::new(true, &snd_args)
        .expect("sender args")
        .runner_config();

    let rcv_thr = thread::spawn(move || {
        let mut reporter = SilentReporter;
        run_receiver_with_reporter(rcv_config, &mut reporter, Some(20)).expect("receiver run");
    });

    thread::sleep(Duration::from_millis(50));

    let snd_thr = thread::spawn(move || {
        let mut reporter = SilentReporter;
        run_sender_with_reporter(snd_config, &mut reporter, Some(20)).expect("sender run");
    });

    snd_thr.join().expect("sender join");
    rcv_thr.join().expect("receiver join");
}
