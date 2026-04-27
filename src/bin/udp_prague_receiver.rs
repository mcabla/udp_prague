//! UDP Prague receiver example.
//!
//! This is a near-literal port of `udp_prague_receiver.cpp`.

use udp_prague::core::run_receiver;
use udp_prague::demo::AppStuff;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let app = match AppStuff::new(false, &args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run_receiver(app, None) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
