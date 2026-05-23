//! quran-tui library crate.
//!
//! The binary (`main.rs`) is a thin shell over this library. The lib/bin split
//! exists so integration tests under `tests/` can reach the modules directly.

pub mod app;
pub mod audio;
pub mod config;
pub mod content;
#[cfg(unix)]
pub mod daemon;
pub mod domain;
pub mod event;
pub mod model;
pub mod ui;
