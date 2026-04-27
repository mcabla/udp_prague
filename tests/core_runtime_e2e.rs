use std::thread;
use std::time::Duration;

use udp_prague::core::{
    run_receiver_with_reporter, run_sender_with_reporter, Reporter, RunnerConfig, RunnerError,
};

struct SilentReporter;

impl Reporter for SilentReporter {}

#[test]
fn sender_receiver_core_runtime_localhost_smoke() {
    let port = 38083u16;

    let rcv_config = RunnerConfig {
        rcv_addr: "0.0.0.0".to_string(),
        rcv_port: u32::from(port),
        ..RunnerConfig::default()
    };
    let snd_config = RunnerConfig {
        rcv_addr: "127.0.0.1".to_string(),
        rcv_port: u32::from(port),
        connect: true,
        ..RunnerConfig::default()
    };

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

#[test]
fn sender_non_connected_startup_timeout_returns_error() {
    let mut reporter = SilentReporter;
    let config = RunnerConfig {
        rcv_addr: "0.0.0.0".to_string(),
        rcv_port: 38089,
        startup_wait_timeout_us: Some(10_000),
        ..RunnerConfig::default()
    };

    let err = run_sender_with_reporter(config, &mut reporter, None).expect_err("startup timeout");
    match err {
        RunnerError::StartupTriggerTimeout { waited_us } => assert_eq!(waited_us, 10_000),
        other => panic!("unexpected error: {other}"),
    }
}
