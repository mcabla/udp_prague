//! UDP Prague sender example.
//!
//! This is a near-literal port of `udp_prague_sender.cpp`.

use udp_prague::core::run_sender;
use udp_prague::demo::AppStuff;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let app = match AppStuff::new(true, &args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = run_sender(app, None) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
