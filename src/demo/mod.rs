//! Demo application compatibility layer.
//!
//! This module holds the CLI/config/reporting code and the JSON writer used by
//! the reference-style example binaries. Library users can disable the
//! `demo-app` feature to exclude this surface entirely.

pub mod app;
pub mod json_writer;

pub use self::app::{AppStuff, REPT_PERIOD};
